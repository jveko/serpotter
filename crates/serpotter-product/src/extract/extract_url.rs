//! Single-URL extract chain (Firecrawl / Tavily).

use serpotter_providers::{ExtractResult, ProviderError, SVC_EXA, SVC_FIRECRAWL, SVC_TAVILY};

use crate::dto::ExtractResponse;
use crate::error::ExtractError;
use crate::lease::{with_key_proxy, LeaseError, ReportMode};
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::search::{is_exhausted_status, is_firecrawl_banned};
use crate::ProductCtx;

pub async fn extract_url(
    ctx: &ProductCtx,
    url: &str,
    preferred: Option<&str>,
) -> Result<ProductOutcome<ExtractResponse>, ProductOutcome<ExtractError>> {
    let url = match crate::ssrf::validate_extract_url(url) {
        Ok(u) => u,
        Err(e) => {
            return Err(ProductOutcome {
                result: e,
                meta: ExecMeta::default(),
            });
        }
    };
    let url = url.as_str();

    // B1: exact-query TTL cache (fail-open). Key = URL + provider choice;
    // structured extract uses its own key (prompt/schema included).
    let canonical = crate::cache::canonical_extract(url, preferred, None, None);
    if let Some(json) =
        crate::cache::cache_get(ctx, crate::cache::SERVICE_EXTRACT, &canonical).await
    {
        if let Ok(resp) = serde_json::from_str::<crate::dto::ExtractResponse>(&json) {
            let mut meta = ExecMeta::default();
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    let chain: Vec<&str> = match preferred {
        Some("tavily") => vec![SVC_TAVILY, SVC_FIRECRAWL],
        Some("exa") => vec![SVC_EXA, SVC_FIRECRAWL, SVC_TAVILY],
        Some("firecrawl") | None => vec![SVC_FIRECRAWL, SVC_TAVILY],
        Some(other) => {
            return Err(ProductOutcome {
                result: ExtractError::Provider(format!("unknown extract provider {other}")),
                meta: ExecMeta::default(),
            });
        }
    };

    let mut meta = ExecMeta::default();
    let mut last = ExtractError::NoHealthyKey("No healthy extract key".into());
    for (i, provider) in chain.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: chain[i - 1].to_string(),
                to: provider.to_string(),
                reason: last.to_string(),
            });
        }
        match try_extract_provider(ctx, provider, url).await {
            Ok(o) => {
                meta.absorb(o.meta);
                let resp = to_response(o.result);
                // B1: cache only successful responses (fail-open on DB errors).
                if let Ok(json) = serde_json::to_string(&resp) {
                    crate::cache::cache_put(ctx, crate::cache::SERVICE_EXTRACT, &canonical, &json)
                        .await;
                }
                return Ok(ProductOutcome { result: resp, meta });
            }
            Err(o) => {
                meta.absorb(o.meta);
                last = o.result;
            }
        }
    }
    Err(ProductOutcome { result: last, meta })
}

/// Extract-chain (url chain) error → mode mapping.
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

/// Per-class failure message strings (pinned by api/product tests). Shared
/// with the research social leg (`extract/research.rs`).
pub(super) fn map_provider_error(provider: &str, e: &ProviderError) -> ExtractError {
    match e {
        ProviderError::Unextractable { message, .. } => {
            ExtractError::Provider(format!("{provider} unextractable: {message}"))
        }
        ProviderError::Unsupported {
            provider,
            action,
            detail,
        } => ExtractError::Provider(format!("{provider} {action} unsupported: {detail}")),
        ProviderError::Upstream { status, .. } if is_exhausted_status(provider, *status) => {
            ExtractError::Provider(format!(
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
            ExtractError::Provider(format!("{provider} temporarily unavailable"))
        }
        ProviderError::Upstream { status, .. } => {
            ExtractError::Provider(format!("{provider} upstream error (status {status})"))
        }
        ProviderError::Http(e) => ExtractError::Provider(format!("{provider} request failed: {e}")),
    }
}

/// Lease-acquire failure → [`ExtractError`] (message strings unchanged;
/// shared by every extract leg).
fn map_extract_lease_err(e: LeaseError) -> ExtractError {
    match e {
        // LeaseError already carries the fully-formatted message
        // ("No healthy {s} key", "All {s} keys busy (acquire timeout)",
        // "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)").
        LeaseError::NoHealthyKey(s) => ExtractError::NoHealthyKey(s),
        LeaseError::KeyBusy(s) => ExtractError::KeyBusy(s),
        LeaseError::NoHealthyNode(msg) => ExtractError::NoHealthyNode(msg),
        LeaseError::Db(e) => ExtractError::Db(e),
    }
}

async fn try_extract_provider(
    ctx: &ProductCtx,
    provider: &str,
    url: &str,
) -> Result<ProductOutcome<ExtractResult>, ProductOutcome<ExtractError>> {
    const MAX_ATTEMPTS: u32 = 3;

    let mut meta = ExecMeta::default();
    let mut last = ExtractError::Provider(format!("{provider}: all attempts failed"));

    for attempt in 1..=MAX_ATTEMPTS {
        // The ladder owns Attempt emission, the provider_attempt span, the
        // http client, hold finishing (per report mode) and meta.note_attempt.
        let outcome = with_key_proxy(
            ctx,
            provider,
            false, // extract providers are web-only (no xAI): the outbound ladder always runs.
            attempt,
            MAX_ATTEMPTS,
            &mut meta,
            map_extract_lease_err,
            |e| report_mode(provider, e),
            |api_key, proxy_url, _http, _hold, _proxy_hold| async move {
                ctx.providers
                    .extract(provider, url, &api_key, proxy_url.as_deref())
                    .await
            },
        )
        .await;

        match outcome {
            Ok(Ok(r)) => {
                // B2: capture the extract cost estimate carried on the result
                // (I2: Exa costDollars / Tavily-Firecrawl 1-credit ESTIMATE).
                // Extract endpoints report no token usage → tokens stay None.
                meta.set_usage(None, None, None, r.cost);
                return Ok(ProductOutcome { result: r, meta });
            }
            Ok(Err(e)) => {
                let mode = report_mode(provider, &e);
                last = map_provider_error(provider, &e);
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
                // retry the SAME account up to MAX_ATTEMPTS. The outer
                // extract_url chain continues to the next provider on Err.
                if matches!(
                    mode,
                    ReportMode::Retryable | ReportMode::Banned | ReportMode::AuthFailure
                ) && attempt < MAX_ATTEMPTS
                {
                    ctx.emit(&ProgressEvent::Retry {
                        service: provider.to_string(),
                        attempt,
                        reason: last.to_string(),
                    });
                    continue;
                }
                return Err(ProductOutcome { result: last, meta });
            }
            Err(e) => {
                // Acquire-side failure (no healthy key / all busy / no node / db).
                return Err(ProductOutcome { result: e, meta });
            }
        }
    }
    Err(ProductOutcome { result: last, meta })
}

fn to_response(r: ExtractResult) -> ExtractResponse {
    ExtractResponse {
        url: r.url,
        title: r.title,
        content: r.content,
        provider_used: r.provider,
        data: None,
        pages: None,
    }
}

/// B18: structured extraction via Firecrawl `/v2/extract` — an async vendor
/// job started in-request and polled every 2s until terminal or
/// `min(request_timeout, 90s)` elapses (the F10 handler deadline is the outer
/// cap; this inner budget is the poll window). No async-job store (B16
/// deliberately not built): the job handle lives only for this request.
///
/// Firecrawl is the only structured backend: `preferred` must be `None` /
/// `Some("auto")` (→ firecrawl) or `Some("firecrawl")`. An explicit
/// non-firecrawl provider is a client error (`InvalidRequest`, 400) — never a
/// provider 5xx.
pub async fn extract_structured(
    ctx: &ProductCtx,
    url: &str,
    prompt: Option<&str>,
    schema: Option<&serde_json::Value>,
    preferred: Option<&str>,
) -> Result<ProductOutcome<ExtractResponse>, ProductOutcome<ExtractError>> {
    match preferred {
        None | Some("auto") | Some("firecrawl") => {}
        Some(other) => {
            return Err(ProductOutcome {
                result: ExtractError::InvalidRequest(format!(
                    "structured extraction requires provider=firecrawl (got {other})"
                )),
                meta: ExecMeta::default(),
            });
        }
    }

    let url = match crate::ssrf::validate_extract_url(url) {
        Ok(u) => u,
        Err(e) => {
            return Err(ProductOutcome {
                result: e,
                meta: ExecMeta::default(),
            });
        }
    };

    // B1: exact-query TTL cache — structured extraction is a long vendor poll,
    // so a repeat (same URL + prompt + schema) pays nothing. Fail-open.
    let canonical = crate::cache::canonical_extract(url.as_str(), preferred, prompt, schema);
    if let Some(json) =
        crate::cache::cache_get(ctx, crate::cache::SERVICE_EXTRACT, &canonical).await
    {
        if let Ok(resp) = serde_json::from_str::<crate::dto::ExtractResponse>(&json) {
            let mut meta = ExecMeta::default();
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    let mut meta = ExecMeta::default();
    // Poll window: min(request_timeout, 90s) — the F10 handler deadline is the
    // outer cap; this inner budget is the poll window.
    let poll_budget = ctx.request_timeout.min(std::time::Duration::from_secs(90));
    // The closure owns the http client and api key, so it can run the whole
    // vendor-job poll; the ladder finishes the holds once at the end. Every
    // provider error maps to Failure (release/release) — the same net effect
    // as the per-exit-path releases this replaces.
    let url_for_call = url.clone();
    let prompt_owned = prompt.map(str::to_string);
    let schema_owned = schema.cloned();
    let outcome = with_key_proxy(
        ctx,
        SVC_FIRECRAWL,
        false,
        1,
        1,
        &mut meta,
        map_extract_lease_err,
        |_| ReportMode::Failure, // structured: every provider error releases both holds
        move |api_key, _proxy_url, http, key_refresh, proxy_refresh| async move {
            let start = ctx
                .providers
                .firecrawl
                .extract_structured(
                    &http,
                    std::slice::from_ref(&url_for_call),
                    prompt_owned.as_deref(),
                    schema_owned.as_ref(),
                    &api_key,
                )
                .await?;
            let deadline = std::time::Instant::now() + poll_budget;
            loop {
                match ctx
                    .providers
                    .firecrawl
                    .structured_status(&http, &start.id, &api_key)
                    .await
                {
                    Ok(st) if st.completed => {
                        return Ok(StructuredOutcome::Completed(st.data));
                    }
                    Ok(st) if st.failed => {
                        return Ok(StructuredOutcome::VendorFailed(
                            st.error.unwrap_or_else(|| "vendor job failed".into()),
                        ));
                    }
                    Ok(_) => {
                        // C3a: still processing — refresh the key + node
                        // leases EVERY poll tick (before the 2s sleep) so the
                        // ~90s poll never lets lease_until expire under the
                        // in-flight hold. Best-effort: a failed refresh never
                        // aborts the poll.
                        key_refresh.refresh().await;
                        if let Some(ph) = &proxy_refresh {
                            ph.refresh().await;
                        }
                        // keep polling while time remains
                        if std::time::Instant::now() >= deadline {
                            return Ok(StructuredOutcome::TimedOut);
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        },
    )
    .await;

    match outcome {
        // Completed: the vendor poll ended in `completed` — success.
        Ok(Ok(StructuredOutcome::Completed(data))) => {
            let resp = ExtractResponse {
                url: url.clone(),
                title: None,
                content: format!("Structured extraction for {url} — see `data`."),
                provider_used: "firecrawl".into(),
                data,
                pages: None,
            };
            // B1: cache only successful responses (fail-open on DB errors).
            if let Ok(json) = serde_json::to_string(&resp) {
                crate::cache::cache_put(ctx, crate::cache::SERVICE_EXTRACT, &canonical, &json)
                    .await;
            }
            Ok(ProductOutcome { result: resp, meta })
        }
        // Completed but vendor-terminal `failed`/`cancelled`: provider error.
        Ok(Ok(StructuredOutcome::VendorFailed(msg))) => Err(ProductOutcome {
            result: ExtractError::Provider(format!(
                "firecrawl structured extraction failed: {msg}"
            )),
            meta,
        }),
        // Poll window elapsed without a terminal state.
        Ok(Ok(StructuredOutcome::TimedOut)) => Err(ProductOutcome {
            result: ExtractError::ExtractTimeout(format!(
                "firecrawl structured extraction did not finish within {}s",
                poll_budget.as_secs()
            )),
            meta,
        }),
        // Provider-call failure (client build / start / status poll): the
        // ladder already finished the holds with Failure semantics.
        Ok(Err(e)) => Err(ProductOutcome {
            result: structured_provider_err("firecrawl structured", e),
            meta,
        }),
        // Acquire-side failure (no healthy key / all busy / no node / db).
        Err(e) => Err(ProductOutcome { result: e, meta }),
    }
}

/// Terminal outcome of the structured-extraction poll loop, surfaced from the
/// ladder call closure (the ladder's error channel is reserved for provider
/// failures; vendor-terminal states and the poll deadline are NOT provider
/// errors).
enum StructuredOutcome {
    /// Job completed; vendor data (absent when the job carried none).
    Completed(Option<serde_json::Value>),
    /// Job reached a terminal `failed`/`cancelled` state; vendor message.
    VendorFailed(String),
    /// The inner poll budget elapsed while the job was still processing.
    TimedOut,
}

/// Map a provider error from the structured path into an honest
/// [`ExtractError::Provider`] message (upstream status preserved). Shared with
/// the tavily-research backend (`extract/research.rs`).
pub(super) fn structured_provider_err(context: &str, e: ProviderError) -> ExtractError {
    ExtractError::Provider(match e {
        ProviderError::Upstream { status, .. } => {
            format!("{context} upstream error (status {status})")
        }
        ProviderError::Http(err) => format!("{context} request failed: {err}"),
        other => format!("{context} failed: {other}"),
    })
}

// ===========================================================================
// B26/B27: batch extract + question/highlights dispatch (single seam for REST
// and MCP — handlers build an ExtractRequest and call [`extract_dispatch`]).
// ===========================================================================

/// Dispatch one [`crate::dto::ExtractRequest`] to the correct product path:
///
/// - `urls` present (non-empty) → B26 batch extract (tavily or exa);
/// - `format=question` → B27 single-URL question (firecrawl);
/// - `format=highlights` → B27 single-URL highlights (exa);
/// - `format=markdown|text` → Tavily `/extract` format passthrough;
/// - `prompt`/`schema`/`output_schema` → B18 structured extract (firecrawl);
/// - otherwise → the plain single-URL scrape chain.
///
/// All paths share the B1 exact-query cache and the request-deadline contract.
pub async fn extract_dispatch(
    ctx: &ProductCtx,
    req: crate::dto::ExtractRequest,
) -> Result<ProductOutcome<crate::dto::ExtractResponse>, ProductOutcome<ExtractError>> {
    let preferred = req.provider.as_deref().filter(|p| *p != "auto");
    let batch = req.urls.as_deref().filter(|u| !u.is_empty());

    if let Some(urls) = batch {
        // Batch modes are single-URL-only for question/highlights.
        if req
            .format
            .as_deref()
            .is_some_and(|f| matches!(f, "question" | "highlights"))
        {
            return Err(ProductOutcome {
                result: ExtractError::InvalidRequest(format!(
                    "format={} requires a single url (batch urls are not supported)",
                    req.format.as_deref().unwrap_or("")
                )),
                meta: ExecMeta::default(),
            });
        }
        return extract_batch_dispatch(ctx, urls, req.format.as_deref(), preferred).await;
    }
    if req.url.trim().is_empty() {
        return Err(ProductOutcome {
            result: ExtractError::InvalidRequest("missing url".into()),
            meta: ExecMeta::default(),
        });
    }
    match req.format.as_deref() {
        Some("question") => {
            return extract_question_dispatch(
                ctx,
                req.url.trim(),
                req.question.as_deref(),
                preferred,
            )
            .await;
        }
        Some("highlights") => {
            return extract_highlights_dispatch(ctx, req.url.trim(), preferred).await;
        }
        Some("markdown") | Some("text") | None => {}
        Some(other) => {
            return Err(ProductOutcome {
                result: ExtractError::InvalidRequest(format!(
                    "format {other:?} is not supported (valid: question, highlights, markdown, text)"
                )),
                meta: ExecMeta::default(),
            });
        }
    }
    if req.prompt.is_some() || req.schema.is_some() || req.output_schema.is_some() {
        let schema = req.schema.as_ref().or(req.output_schema.as_ref());
        return extract_structured(
            ctx,
            req.url.trim(),
            req.prompt.as_deref(),
            schema,
            preferred,
        )
        .await;
    }
    extract_url(ctx, req.url.trim(), preferred).await
}

/// B26 batch extract: one vendor call for many URLs. Backends: tavily
/// (`provider` tavily/auto; `format` markdown|text passthrough) or exa
/// (`provider=exa`; format ignored — exa returns page text). Firecrawl has no
/// batch surface → explicit provider=firecrawl is a client error.
async fn extract_batch_dispatch(
    ctx: &ProductCtx,
    urls: &[String],
    format: Option<&str>,
    preferred: Option<&str>,
) -> Result<ProductOutcome<crate::dto::ExtractResponse>, ProductOutcome<ExtractError>> {
    let canonical = crate::cache::canonical_extract_v2(urls, preferred, format, None, None);
    if let Some(json) =
        crate::cache::cache_get(ctx, crate::cache::SERVICE_EXTRACT, &canonical).await
    {
        if let Ok(resp) = serde_json::from_str::<crate::dto::ExtractResponse>(&json) {
            let mut meta = ExecMeta::default();
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    let mut meta = ExecMeta::default();
    // Provider dispatch: exa explicitly → exa; anything else (auto/tavily/
    // unset) → tavily; firecrawl is not a batch backend.
    if preferred == Some(SVC_EXA) {
        let out = batch_via(ctx, SVC_EXA, urls, format, &mut meta)
            .await
            .map_err(|result| ProductOutcome {
                result,
                meta: meta.clone(),
            })?;
        let resp = batch_to_response(out, SVC_EXA);
        crate::cache::cache_put(
            ctx,
            crate::cache::SERVICE_EXTRACT,
            &canonical,
            &resp_json(&resp),
        )
        .await;
        return Ok(ProductOutcome { result: resp, meta });
    }
    match preferred {
        Some("firecrawl") => Err(ProductOutcome {
            result: ExtractError::InvalidRequest(
                "batch extract (urls) supports provider=tavily or provider=exa (firecrawl has no batch endpoint)"
                    .into(),
            ),
            meta,
        }),
        _ => {
            let out = batch_via(ctx, SVC_TAVILY, urls, format, &mut meta).await.map_err(|result| ProductOutcome { result, meta: meta.clone() })?;
            let resp = batch_to_response(out, SVC_TAVILY);
            crate::cache::cache_put(ctx, crate::cache::SERVICE_EXTRACT, &canonical, &resp_json(&resp))
                .await;
            Ok(ProductOutcome { result: resp, meta })
        }
    }
}

/// Run one provider's batch-extract client method on the dual-pool ladder
/// with a single attempt (batch calls are atomic vendor calls — no retry:
/// the vendor already fails per-URL internally). Every provider error maps
/// to [`ReportMode::Failure`] (release/release — the current
/// release-on-every-error behavior).
async fn batch_via(
    ctx: &ProductCtx,
    provider: &str,
    urls: &[String],
    format: Option<&str>,
    meta: &mut ExecMeta,
) -> Result<Vec<crate::dto::ExtractedPageBrief>, ExtractError> {
    let outcome = with_key_proxy(
        ctx,
        provider,
        false, // batch extract is a web-provider call: the outbound ladder always runs.
        1,
        1,
        meta,
        map_extract_lease_err,
        |_| ReportMode::Failure, // batch: every provider error releases both holds
        |api_key, _proxy_url, http, _hold, _proxy_hold| async move {
            match provider {
                SVC_TAVILY => ctx
                    .providers
                    .tavily
                    .extract_batch(&http, &api_key, urls, format)
                    .await
                    .map(|pages| {
                        pages
                            .into_iter()
                            .map(|p| crate::dto::ExtractedPageBrief {
                                url: p.url,
                                content: p.content,
                            })
                            .collect::<Vec<_>>()
                    }),
                SVC_EXA => ctx
                    .providers
                    .exa
                    .extract_batch(&http, &api_key, urls)
                    .await
                    .map(|pages| {
                        pages
                            .into_iter()
                            .map(|p| crate::dto::ExtractedPageBrief {
                                url: p.url,
                                content: p.content,
                            })
                            .collect::<Vec<_>>()
                    }),
                other => Err(ProviderError::Unsupported {
                    provider: other.to_string(),
                    action: "extract_batch",
                    detail: "batch extract unsupported".into(),
                }),
            }
        },
    )
    .await;

    match outcome {
        Ok(Ok(pages)) => Ok(pages),
        Ok(Err(e)) => Err(map_batch_provider_error(provider, &e)),
        // Acquire-side failure (no healthy key / all busy / no node / db).
        Err(e) => Err(e),
    }
}

/// Per-class batch-extract failure messages (pinned by api/product tests).
fn map_batch_provider_error(provider: &str, e: &ProviderError) -> ExtractError {
    match e {
        ProviderError::Unextractable { message, .. } => {
            ExtractError::Provider(format!("{provider} batch unextractable: {message}"))
        }
        ProviderError::Unsupported {
            provider,
            action,
            detail,
        } => ExtractError::InvalidRequest(format!("{provider} {action} unsupported: {detail}")),
        ProviderError::Upstream { status, .. } => {
            ExtractError::Provider(format!("{provider} upstream error (status {status})"))
        }
        ProviderError::Http(err) => {
            ExtractError::Provider(format!("{provider} request failed: {err}"))
        }
    }
}

fn resp_json(resp: &crate::dto::ExtractResponse) -> String {
    serde_json::to_string(resp).unwrap_or_default()
}

/// Batch responses keep the top-level `url`/`content` on the FIRST page for
/// wire compatibility and carry the full list in `pages`.
fn batch_to_response(
    pages: Vec<crate::dto::ExtractedPageBrief>,
    provider: &str,
) -> crate::dto::ExtractResponse {
    let first = pages.first();
    crate::dto::ExtractResponse {
        url: first.map(|p| p.url.clone()).unwrap_or_default(),
        title: None,
        content: first.map(|p| p.content.clone()).unwrap_or_default(),
        provider_used: provider.into(),
        data: None,
        pages: Some(pages),
    }
}

/// B27 question extraction: one question answered from ONE URL via Firecrawl
/// `/v2/extract` (the only question backend — the product layer gates
/// provider here as a 400 client error, never a provider 5xx).
async fn extract_question_dispatch(
    ctx: &ProductCtx,
    url: &str,
    question: Option<&str>,
    preferred: Option<&str>,
) -> Result<ProductOutcome<crate::dto::ExtractResponse>, ProductOutcome<ExtractError>> {
    match preferred {
        None | Some("firecrawl") => {}
        Some(other) => {
            return Err(ProductOutcome {
                result: ExtractError::InvalidRequest(format!(
                    "format=question requires provider=firecrawl (got {other})"
                )),
                meta: ExecMeta::default(),
            });
        }
    }
    let Some(question) = question.filter(|q| !q.trim().is_empty()) else {
        return Err(ProductOutcome {
            result: ExtractError::InvalidRequest("format=question requires a question".into()),
            meta: ExecMeta::default(),
        });
    };
    let url = match crate::ssrf::validate_extract_url(url) {
        Ok(u) => u,
        Err(e) => {
            return Err(ProductOutcome {
                result: e,
                meta: ExecMeta::default(),
            });
        }
    };

    let canonical = crate::cache::canonical_extract_v2(
        std::slice::from_ref(&url.as_str().to_string()),
        Some("firecrawl"),
        Some("question"),
        Some(question),
        None,
    );
    if let Some(json) =
        crate::cache::cache_get(ctx, crate::cache::SERVICE_EXTRACT, &canonical).await
    {
        if let Ok(resp) = serde_json::from_str::<crate::dto::ExtractResponse>(&json) {
            let mut meta = ExecMeta::default();
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    let mut meta = ExecMeta::default();
    // Single-call ladder: every provider error maps to Failure (release both
    // holds — the current release-on-every-error behavior).
    let url_for_call = url.clone();
    let question_owned = question.to_string();
    let outcome = with_key_proxy(
        ctx,
        SVC_FIRECRAWL,
        false,
        1,
        1,
        &mut meta,
        map_extract_lease_err,
        |_| ReportMode::Failure, // question: every provider error releases both holds
        move |api_key, _proxy_url, http, _hold, _proxy_hold| async move {
            ctx.providers
                .firecrawl
                .extract_question(&http, &api_key, &url_for_call, &question_owned)
                .await
        },
    )
    .await;

    match outcome {
        Ok(Ok(data)) => {
            let resp = crate::dto::ExtractResponse {
                url: url.to_string(),
                title: None,
                content: "Question extraction for {url} — see `data`."
                    .replace("{url}", url.as_str()),
                provider_used: "firecrawl".into(),
                data: Some(data),
                pages: None,
            };
            crate::cache::cache_put(
                ctx,
                crate::cache::SERVICE_EXTRACT,
                &canonical,
                &resp_json(&resp),
            )
            .await;
            Ok(ProductOutcome { result: resp, meta })
        }
        Ok(Err(e)) => Err(ProductOutcome {
            result: structured_provider_err("firecrawl question extraction", e),
            meta,
        }),
        // Acquire-side failure (no healthy key / all busy / no node / db).
        Err(e) => Err(ProductOutcome { result: e, meta }),
    }
}

/// B27 highlights extraction: the page's key sentences via Exa `/contents`
/// (the only highlights backend — provider gated here as a 400 client error).
async fn extract_highlights_dispatch(
    ctx: &ProductCtx,
    url: &str,
    preferred: Option<&str>,
) -> Result<ProductOutcome<crate::dto::ExtractResponse>, ProductOutcome<ExtractError>> {
    match preferred {
        None | Some("exa") => {}
        Some(other) => {
            return Err(ProductOutcome {
                result: ExtractError::InvalidRequest(format!(
                    "format=highlights requires provider=exa (got {other})"
                )),
                meta: ExecMeta::default(),
            });
        }
    }
    let url = match crate::ssrf::validate_extract_url(url) {
        Ok(u) => u,
        Err(e) => {
            return Err(ProductOutcome {
                result: e,
                meta: ExecMeta::default(),
            });
        }
    };

    let canonical = crate::cache::canonical_extract_v2(
        std::slice::from_ref(&url.as_str().to_string()),
        Some("exa"),
        Some("highlights"),
        None,
        None,
    );
    if let Some(json) =
        crate::cache::cache_get(ctx, crate::cache::SERVICE_EXTRACT, &canonical).await
    {
        if let Ok(resp) = serde_json::from_str::<crate::dto::ExtractResponse>(&json) {
            let mut meta = ExecMeta::default();
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    let mut meta = ExecMeta::default();
    // Single-call ladder: every provider error maps to Failure (release both
    // holds — the current release-on-every-error behavior).
    let url_for_call = url.clone();
    let outcome = with_key_proxy(
        ctx,
        SVC_EXA,
        false,
        1,
        1,
        &mut meta,
        map_extract_lease_err,
        |_| ReportMode::Failure, // highlights: every provider error releases both holds
        move |api_key, _proxy_url, http, _hold, _proxy_hold| async move {
            ctx.providers
                .exa
                .extract_highlights(&http, &api_key, &url_for_call)
                .await
        },
    )
    .await;

    match outcome {
        Ok(Ok(content)) => {
            let resp = crate::dto::ExtractResponse {
                url: url.to_string(),
                title: None,
                content,
                provider_used: "exa".into(),
                data: None,
                pages: None,
            };
            crate::cache::cache_put(
                ctx,
                crate::cache::SERVICE_EXTRACT,
                &canonical,
                &resp_json(&resp),
            )
            .await;
            Ok(ProductOutcome { result: resp, meta })
        }
        Ok(Err(e)) => Err(ProductOutcome {
            result: structured_provider_err("exa highlights extraction", e),
            meta,
        }),
        // Acquire-side failure (no healthy key / all busy / no node / db).
        Err(e) => Err(ProductOutcome { result: e, meta }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::{Arc, Mutex};

    use serpotter_db::Db;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
    };

    use crate::meta::{ProgressEvent, ProgressSink};
    use crate::ProductCtx;

    #[derive(Default, Clone)]
    struct VecSink(Arc<Mutex<Vec<ProgressEvent>>>);

    impl ProgressSink for VecSink {
        fn emit(&self, event: &ProgressEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    async fn test_db() -> Db {
        serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate")
    }

    /// Standard ctx: every provider points at `127.0.0.1:9` (connection
    /// refused), no outbound nodes, `require_proxy=false`.
    fn ctx_for(db: Db, sink: VecSink) -> ProductCtx {
        let keys = Arc::new(KeyPool::new(db.clone()));
        let outbound = Arc::new(ProxyPool::with_options(db.clone(), false));
        ProductCtx {
            db,
            keys,
            outbound,
            providers: ProviderRegistry::with_clients(
                TavilyClient::new("http://127.0.0.1:9"),
                FirecrawlClient::new("http://127.0.0.1:9"),
                ExaClient::new("http://127.0.0.1:9"),
                XaiClient::new("http://127.0.0.1:9"),
            ),
            progress: Some(Arc::new(sink)),
            request_timeout: std::time::Duration::from_secs(120),
            cache_enabled: false,
            cache_ttl: std::time::Duration::from_secs(300),
        }
    }

    /// Same as `ctx_for` but with firecrawl pointed at a loopback mock.
    fn ctx_for_firecrawl_mock(db: Db, sink: VecSink, mock_url: String) -> ProductCtx {
        let mut ctx = ctx_for(db, sink);
        ctx.providers = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new(mock_url),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        ctx
    }

    /// Minimal loopback mock: serves a canned 200 JSON per request path, then
    /// closes the connection (reqwest opens a fresh connection per attempt).
    fn spawn_mock_extract(routes: &[(&'static str, &'static str)]) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let routes = routes.to_vec();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            let head_end = find_seq(&buf, b"\r\n\r\n");
                            let Some(hl) = head_end else { continue };
                            let head = String::from_utf8_lossy(&buf[..hl]).to_string();
                            let cl = head.lines().find_map(|l| {
                                let lower = l.to_ascii_lowercase();
                                lower
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            });
                            match cl {
                                Some(len) if buf.len() >= hl + 4 + len => break,
                                Some(_) => continue,
                                None => break,
                            }
                        }
                        Err(_) => break,
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/");
                let body = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, b)| *b)
                    .unwrap_or(r#"{"error":"no route"}"#);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    fn find_seq(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// The url-chain leg runs on the dual-pool ladder: one Attempt per retry
    /// round with honest `max`, Retry events between attempts, the outer chain
    /// Fallback to the next provider, released holds (transport never
    /// fail@3s), and a final error that names the missing chain provider.
    #[tokio::test]
    async fn url_chain_ladder_fallback_order_and_events() {
        let db = test_db().await;
        let key = db
            .insert_api_key("firecrawl", "fc-ladder-test")
            .await
            .unwrap();
        let sink = VecSink::default();
        let ctx = ctx_for(db.clone(), sink.clone());
        // Preferred=firecrawl: chain is [firecrawl, tavily]; connection refused
        // → retryable Http failures ×3, then Fallback to tavily (no key →
        // NoHealthyKey).
        let err = crate::extract_url(&ctx, "https://example.com", Some("firecrawl"))
            .await
            .expect_err("chain must fail with no healthy tavily key");
        assert!(
            matches!(&err.result, crate::ExtractError::NoHealthyKey(m) if m.contains("tavily")),
            "final error names the missing chain provider: {:?}",
            err.result
        );

        let events = sink.0.lock().unwrap().clone();
        // Ladder Attempts: firecrawl 1..=3, max 3.
        let attempts: Vec<&ProgressEvent> = events
            .iter()
            .filter(
                |e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "firecrawl"),
            )
            .collect();
        assert_eq!(
            attempts.len(),
            3,
            "one ladder Attempt per round: {events:?}"
        );
        assert_eq!(
            attempts[0],
            &ProgressEvent::Attempt {
                service: "firecrawl".into(),
                attempt: 1,
                max: 3
            }
        );
        assert_eq!(
            attempts[2],
            &ProgressEvent::Attempt {
                service: "firecrawl".into(),
                attempt: 3,
                max: 3
            }
        );
        // Two Retries after the first two failures, naming service + attempt.
        let retries: Vec<&ProgressEvent> = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
            .collect();
        assert_eq!(
            retries.len(),
            2,
            "two retries after two failures: {events:?}"
        );
        assert!(matches!(
            retries[0],
            ProgressEvent::Retry { service, attempt: 1, .. } if service == "firecrawl"
        ));
        // Interleaving: Attempt1, Retry1, Attempt2, Retry2, Attempt3.
        assert!(
            matches!(&events[0], ProgressEvent::Attempt { service, attempt: 1, .. } if service == "firecrawl")
        );
        assert!(
            matches!(&events[1], ProgressEvent::Retry { service, attempt: 1, .. } if service == "firecrawl")
        );
        assert!(
            matches!(&events[2], ProgressEvent::Attempt { service, attempt: 2, .. } if service == "firecrawl")
        );
        assert!(
            matches!(&events[3], ProgressEvent::Retry { service, attempt: 2, .. } if service == "firecrawl")
        );
        assert!(
            matches!(&events[4], ProgressEvent::Attempt { service, attempt: 3, .. } if service == "firecrawl")
        );

        // Then the outer chain falls through to tavily.
        let fallbacks: Vec<&ProgressEvent> = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
            .collect();
        assert_eq!(
            fallbacks.len(),
            1,
            "one fallback for a 2-provider chain: {events:?}"
        );
        assert!(
            matches!(fallbacks[0], ProgressEvent::Fallback { from, to, .. } if from == "firecrawl" && to == "tavily"),
            "fallback names the pair: {events:?}"
        );

        // meta records the attempted provider (request_log parity); transport
        // failures released the hold — the key was never fail@3'ed.
        assert_eq!(err.meta.providers_consulted, vec!["firecrawl"]);
        let row = db
            .get_api_key(key.id)
            .await
            .unwrap()
            .expect("firecrawl key row");
        assert_eq!(row.active, 1, "transport must not hard-disable the key");
        assert_eq!(row.consecutive_fails, 0, "transport must not count fails");
    }

    /// The structured-extraction poll loop lives inside the ladder call
    /// closure: a vendor-terminal `failed` status maps to the same Provider
    /// message as before, the hold is released (key stays active), and the
    /// ladder records exactly one attempt.
    #[tokio::test]
    async fn structured_poll_failure_releases_holds_and_maps_same_error() {
        let db = test_db().await;
        let key = db
            .insert_api_key("firecrawl", "fc-struct-ladder")
            .await
            .unwrap();
        let mock = spawn_mock_extract(&[
            ("/v2/extract", r#"{"success":true,"id":"job-1"}"#),
            (
                "/v2/extract/job-1",
                r#"{"success":true,"status":"failed","error":"blocked by robots"}"#,
            ),
        ]);
        let sink = VecSink::default();
        let ctx = ctx_for_firecrawl_mock(db.clone(), sink.clone(), mock);
        let err = crate::extract_structured(
            &ctx,
            "https://example.com",
            Some("extract the company"),
            None,
            None,
        )
        .await
        .expect_err("failed job surfaces as an error");
        let message = match &err.result {
            crate::ExtractError::Provider(m) => m.clone(),
            other => panic!("expected Provider error, got {other:?}"),
        };
        assert!(
            message.starts_with("firecrawl structured extraction failed:"),
            "same message shape as before: {message}"
        );
        assert!(
            message.contains("blocked by robots"),
            "vendor error preserved: {message}"
        );

        // Single ladder attempt recorded (request_log parity) and the hold was
        // released with Failure semantics — never fail@3'ed by a vendor job
        // failure.
        assert_eq!(err.meta.providers_consulted, vec!["firecrawl"]);
        let row = db.get_api_key(key.id).await.unwrap().unwrap();
        assert_eq!(row.active, 1, "poll failure must not hard-disable the key");
        assert_eq!(
            row.consecutive_fails, 0,
            "Failure mode releases, never fails"
        );
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![ProgressEvent::Attempt {
                service: "firecrawl".into(),
                attempt: 1,
                max: 1
            }],
            "one ladder Attempt, nothing else: {events:?}"
        );
    }
}
