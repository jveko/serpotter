//! Single-provider attempt loop (key/proxy dual-pool matrix) on
//! [`crate::lease::with_key_proxy`].

use serpotter_core::SearchQuery;
use serpotter_providers::{ProviderError, ProviderResult, ProviderSearchParams, SVC_XAI};

use crate::error::SearchExecError;
use crate::lease::{with_key_proxy, LeaseError, ReportMode};
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::ProductCtx;

use super::{is_exhausted_status, is_firecrawl_banned};

/// Search-path retry budget (unchanged; pinned by tests).
const MAX_ATTEMPTS: u32 = 3;

/// Search-path error → mode mapping.
///
/// Same classes as the shared `verdict_for`, EXCEPT transport (`Http`) errors
/// are retryable here: connection-refused/transport failures retry the same
/// account (pinned: 3 attempts / 2 retries on `127.0.0.1:9`), while
/// `Unsupported`/`Unextractable` return immediately (report decides the hold
/// finishing only — the retry loop below applies the mode).
fn report_mode(provider: &str, e: &ProviderError) -> ReportMode {
    match e {
        ProviderError::Upstream { status, .. } if is_exhausted_status(provider, *status) => {
            ReportMode::Exhausted
        }
        ProviderError::Upstream { status, body, .. }
            if provider == "firecrawl" && is_firecrawl_banned(*status, body) =>
        {
            ReportMode::Banned
        }
        ProviderError::Upstream { status, .. } if *status == 401 || *status == 403 => {
            ReportMode::AuthFailure
        }
        ProviderError::Upstream { status, .. } if *status == 429 || (500..600).contains(status) => {
            ReportMode::Retryable
        }
        ProviderError::Http(_) => ReportMode::Retryable,
        _ => ReportMode::Failure,
    }
}

/// Per-class failure message strings (pinned by api/product tests).
fn map_provider_error(provider: &str, e: &ProviderError) -> SearchExecError {
    match e {
        ProviderError::Unextractable { message, .. } => {
            SearchExecError::Provider(format!("{provider} unextractable: {message}"))
        }
        ProviderError::Unsupported {
            provider,
            action,
            detail,
        } => SearchExecError::Provider(format!("{provider} {action} unsupported: {detail}")),
        ProviderError::Upstream { status, body, .. } if is_exhausted_status(provider, *status) => {
            SearchExecError::Provider(format!("{provider} exhausted status {status}: {body}"))
        }
        ProviderError::Upstream { status, body, .. }
            if provider == "firecrawl" && is_firecrawl_banned(*status, body) =>
        {
            SearchExecError::Provider(format!("{provider} banned status {status}: {body}"))
        }
        ProviderError::Upstream { status, body, .. } => {
            SearchExecError::Provider(format!("{provider} upstream {status}: {body}"))
        }
        ProviderError::Http(e) => {
            SearchExecError::Search(format!("{provider} request failed: {e}"))
        }
    }
}

/// Lease-acquire failure → [`SearchExecError`]. The ladder already formats the
/// full message into the variant (`"No healthy {s} key"` / `"All {s} keys busy
/// (acquire timeout)"`), so this is a plain passthrough (message strings pinned
/// by api tests). Shared by `run_provider` and the deep-search leg.
pub fn map_lease_err(e: LeaseError) -> SearchExecError {
    match e {
        LeaseError::NoHealthyKey(msg) => SearchExecError::NoHealthyKey(msg),
        LeaseError::KeyBusy(msg) => SearchExecError::KeyBusy(msg),
        LeaseError::NoHealthyNode(msg) => SearchExecError::NoHealthyNode(msg),
        LeaseError::Db(e) => SearchExecError::Db(e),
    }
}

/// Run one provider: lease-one key (+ proxy unless xAI), dual-pool matrix, max 3 attempts.
#[allow(clippy::too_many_arguments)]
pub async fn run_provider(
    ctx: &ProductCtx,
    provider: &str,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
    sources_override: Option<&[String]>,
) -> Result<ProductOutcome<ProviderResult>, ProductOutcome<SearchExecError>> {
    let mut meta = ExecMeta::default();
    let sources = sources_override.or(decision.sources.as_deref());
    let allowed_handles = body
        .allowed_x_handles
        .as_ref()
        .map(|v| v.as_list())
        .filter(|v| !v.is_empty());
    let excluded_handles = body
        .excluded_x_handles
        .as_ref()
        .map(|v| v.as_list())
        .filter(|v| !v.is_empty());
    let mut last_err = SearchExecError::Provider(format!("{provider}: all attempts failed"));

    for attempt in 1..=MAX_ATTEMPTS {
        // The ladder owns Attempt emission, the provider_attempt span, the
        // http client, hold finishing (per report mode) and meta.note_attempt.
        // Params construction lives here so it sees the leased api_key.
        let outcome = with_key_proxy(
            ctx,
            provider,
            provider == SVC_XAI, // xAI never touches outbound.
            attempt,
            MAX_ATTEMPTS,
            &mut meta,
            map_lease_err,
            |e| report_mode(provider, e),
            |api_key, proxy_url, _http| {
                // Copy the handle slices out so the async block captures
                // Copy values (an Option<Vec<String>> would move on attempt 1).
                let allowed = allowed_handles.as_deref();
                let excluded = excluded_handles.as_deref();
                async move {
                    let params = ProviderSearchParams {
                        query: body.query.trim(),
                        max_results,
                        api_key: &api_key,
                        include_content,
                        include_answer: true,
                        // B9 wiring: tavily-only surface — other providers ignore these.
                        include_images: body.include_images,
                        include_raw_content: body.include_raw_content,
                        chunks_per_source: body.chunks_per_source,
                        search_depth: body
                            .search_depth
                            .as_deref()
                            // B20: deep modes (deep-lite|deep|deep-reasoning) select the
                            // Exa server-side embeddings leg, which never flows through
                            // run_provider — a web provider must not receive them upstream
                            // (Tavily would 400 on "deep").
                            .filter(|d| !serpotter_core::is_deep_mode(Some(d))),
                        tavily_topic: decision.tavily_topic.as_deref(),
                        firecrawl_categories: decision.firecrawl_categories.as_deref(),
                        sources,
                        include_domains: if include_domains.is_empty() {
                            None
                        } else {
                            Some(include_domains)
                        },
                        exclude_domains: if exclude_domains.is_empty() {
                            None
                        } else {
                            Some(exclude_domains)
                        },
                        allowed_x_handles: allowed,
                        excluded_x_handles: excluded,
                        from_date: body.from_date.as_deref(),
                        to_date: body.to_date.as_deref(),
                        time_range: body.time_range.as_deref(),
                        country: body.country.as_deref(),
                        exact_match: body.exact_match,
                    };
                    ctx.providers
                        .search(provider, params, proxy_url.as_deref())
                        .await
                }
            },
        )
        .await;

        match outcome {
            Ok(Ok(r)) => {
                return Ok(ProductOutcome { result: r, meta });
            }
            Ok(Err(e)) => {
                let mode = report_mode(provider, &e);
                last_err = map_provider_error(provider, &e);
                if mode == ReportMode::Banned {
                    if let ProviderError::Upstream { status, .. } = &e {
                        tracing::warn!(
                            key_id = meta.key_id,
                            status = *status,
                            reason = "firecrawl_banned",
                            "firecrawl key banned; deleting from pool"
                        );
                    }
                }
                // Exhausted / Unsupported / Unextractable / non-retryable 4xx /
                // Http-as-Failure return immediately; Retryable/Banned/AuthFailure
                // retry the SAME account up to MAX_ATTEMPTS.
                if matches!(
                    mode,
                    ReportMode::Retryable | ReportMode::Banned | ReportMode::AuthFailure
                ) && attempt < MAX_ATTEMPTS
                {
                    ctx.emit(&ProgressEvent::Retry {
                        service: provider.to_string(),
                        attempt,
                        reason: last_err.to_string(),
                    });
                    continue;
                }
                return Err(ProductOutcome {
                    result: last_err,
                    meta,
                });
            }
            Err(e) => {
                // Acquire-side failure (no healthy key / all busy / no node / db).
                return Err(ProductOutcome { result: e, meta });
            }
        }
    }
    Err(ProductOutcome {
        result: last_err,
        meta,
    })
}
