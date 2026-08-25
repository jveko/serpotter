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
        ProviderError::Upstream { status, .. } if is_exhausted_status(provider, *status) => {
            SearchExecError::Provider(format!(
                "{provider} rate-limited (upstream {status}); try again shortly"
            ))
        }
        // Agent-facing messages carry NO vendor response text — even a
        // snippet can contain alarming wording ("key banned", account ids)
        // that derails agent execution. The verbatim body lives only in the
        // server WARN log (`reason=upstream_error` / `firecrawl_banned`).
        ProviderError::Upstream { status, body, .. }
            if provider == "firecrawl" && is_firecrawl_banned(*status, body) =>
        {
            SearchExecError::Provider(format!("{provider} temporarily unavailable"))
        }
        ProviderError::Upstream { status, .. } => {
            SearchExecError::Provider(format!("{provider} upstream error (status {status})"))
        }
        ProviderError::Http(e) => {
            SearchExecError::Provider(format!("{provider} request failed: {e}"))
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

/// Deterministic jittered backoff for retry-class continues (Http / 429 / 5xx /
/// 401 / 403 / banned), so a transient upstream storm doesn't burn all
/// MAX_ATTEMPTS immediately (C2b).
///
/// Exponential base `200ms * 2^(attempt-2)` (floored at 200ms for attempt <= 1)
/// hard-capped at 1000ms, with ±25% jitter (±2 eighths) derived from the
/// attempt number, clamped so the result never exceeds the cap:
///   attempt 2 → ~200ms  (150–250 with jitter)
///   attempt 3 → ~400ms  (300–500 with jitter)
///   attempt ≥ 5 → base capped at 1000ms, value still ≤ 1000ms
/// Deterministic: the same attempt always yields the same delay (unit-tested).
fn retry_backoff_ms(attempt: u32) -> u64 {
    // 200ms * 2^(attempt-2), floored at 200ms, capped at 1000ms.
    let exp = attempt.saturating_sub(2).min(3);
    let base = (200u64 << exp).min(1000);
    // Jitter in eighths: ((attempt * 37) % 5) - 2 ∈ -2..=2 (±25%).
    let delta = ((attempt as i64 * 37) % 5) - 2;
    ((base as i64 * (8 + delta) / 8).clamp(0, 1000)) as u64
}

/// Run one provider: lease-one key (+ proxy unless xAI), dual-pool matrix, max 3 attempts.
/// Retry-class failures sleep a bounded jittered backoff before re-firing.
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
            |api_key, proxy_url, _http, _hold, _proxy_hold| {
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
                if let ProviderError::Upstream { status, body, .. } = &e {
                    if mode == ReportMode::Banned {
                        tracing::warn!(
                            key_id = meta.key_id,
                            status = *status,
                            body = %body,
                            reason = "firecrawl_banned",
                            "firecrawl key banned; deleting from pool"
                        );
                    } else {
                        // Durable full-body record: clients only ever see the
                        // sanitized snippet, so this is the sole place the
                        // verbatim vendor response survives for diagnosis.
                        tracing::warn!(
                            key_id = meta.key_id,
                            provider = provider,
                            status = *status,
                            body = %body,
                            reason = "upstream_error",
                            "provider upstream error; full body logged"
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
                    // Bounded jittered backoff so a transient upstream storm
                    // doesn't burn all attempts immediately (C2b). Only the
                    // provider-call retry classes reach this point; immediate
                    // returns and acquire-side errors never sleep.
                    tokio::time::sleep(std::time::Duration::from_millis(retry_backoff_ms(attempt)))
                        .await;
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

#[cfg(test)]
mod tests {
    use super::retry_backoff_ms;

    #[test]
    fn retry_backoff_ms_first_retry_window() {
        // attempt 2 (first documented retry) → ~200ms, jittered 150–250ms.
        let v = retry_backoff_ms(2);
        assert!(
            (150..=250).contains(&v),
            "attempt 2 backoff {v}ms out of 150–250 window"
        );
    }

    #[test]
    fn retry_backoff_ms_second_retry_window() {
        // attempt 3 → ~400ms, jittered 300–500ms.
        let v = retry_backoff_ms(3);
        assert!(
            (300..=500).contains(&v),
            "attempt 3 backoff {v}ms out of 300–500 window"
        );
    }

    #[test]
    fn retry_backoff_ms_capped_at_one_second() {
        // The cap is reachable: attempt 6 sits exactly on the 1000ms cap.
        assert_eq!(retry_backoff_ms(6), 1000);
        // No attempt in a wide sweep ever exceeds the cap, and none dips
        // below the jitter floor (min possible = 150ms at base 200ms).
        for attempt in 1..=64 {
            let v = retry_backoff_ms(attempt);
            assert!(
                (150..=1000).contains(&v),
                "attempt {attempt} backoff {v}ms out of [150, 1000]"
            );
        }
    }

    #[test]
    fn retry_backoff_ms_deterministic() {
        // Same attempt → same delay (no wall-clock randomness), so the
        // per-attempt bounds stay stable across runs.
        for attempt in 1..=16 {
            assert_eq!(
                retry_backoff_ms(attempt),
                retry_backoff_ms(attempt),
                "attempt {attempt} must be deterministic"
            );
        }
    }
}
