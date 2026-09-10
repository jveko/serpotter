//! Single-URL extract chain (Firecrawl / Tavily).

use serpotter_providers::{ExtractResult, ProviderError, SVC_EXA, SVC_FIRECRAWL, SVC_TAVILY};

use crate::dto::ExtractResponse;
use crate::error::ExtractError;
use crate::lease::{with_key_proxy, LeaseError, ReportMode};
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::search::{is_account_banned, is_exhausted_status};
use crate::ProductCtx;

/// Fold one extract-provider spelling to its canonical member, mirroring
/// core's `fold_member` discipline exactly: rewrite ONLY on
/// `normalize_choice`'s `Ok(Some(canonical))`. A non-member (and blanks,
/// which the boundaries already call unset) survives byte-for-byte so every
/// downstream refusal — `chain_for`'s `Some(other)` arm, the batch gates —
/// still names what the client actually sent. This is formatting, never
/// validation; membership decisions belong to the API boundaries.
fn fold_extract_provider(value: &str) -> String {
    serpotter_core::normalize_choice(
        "provider",
        Some(value),
        serpotter_core::VALID_EXTRACT_PROVIDERS,
    )
    .ok()
    .flatten()
    .unwrap_or_else(|| value.to_string())
}

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
    // `extract_url` is a PUBLIC entry (re-exported from the crate root), not
    // only the tail of `extract_dispatch` — direct callers hand `preferred`
    // in from every direction, so the fold also runs here, before the
    // `canonical_extract` cache key and the exact-match chain arms below.
    // Idempotent on members, so values `extract_dispatch` already folded
    // pass through untouched: one rule, one helper, no second source.
    let preferred = preferred.map(fold_extract_provider);
    let preferred = preferred.as_deref();

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
    // Aggregation rule, identical in shape to `run_chain`'s: a provider-side
    // failure (unreachable vendor, no healthy key, rate limit) outranks a local
    // parameter refusal, because "we could not get an answer out of that
    // provider" is strictly weaker evidence about the REQUEST than "this request
    // shape is refused". A refusal only becomes the caller's 400 when no leg
    // failed provider-side, and then the FIRST refusal is what surfaces (it names
    // the leg that rejected the shape). `Fallback.reason` keeps carrying the
    // previous leg's `Display`, exactly as before.
    let mut refusal: Option<String> = None;
    let mut provider_side_err: Option<ExtractError> = None;
    let mut last_reason = String::from("No healthy extract key");
    for (i, provider) in chain.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: chain[i - 1].to_string(),
                to: provider.to_string(),
                reason: last_reason.clone(),
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
                match o.result {
                    ExtractError::InvalidRequest(m) => {
                        if refusal.is_none() {
                            refusal = Some(m.clone());
                        }
                        last_reason = m;
                    }
                    other => {
                        last_reason = other.to_string();
                        provider_side_err = Some(other);
                    }
                }
            }
        }
    }
    Err(ProductOutcome {
        result: surfaced_extract_err(provider_side_err, refusal),
        meta,
    })
}

/// The extract chain's all-legs-empty choice — the sequential twin of
/// `search::execute::leg_aggregate_err` and `search::chain::run_chain`'s rule.
/// Provider-side first, then the refusal the legs agreed on, then today's
/// no-key default (an empty chain never ran a leg).
fn surfaced_extract_err(
    provider_side: Option<ExtractError>,
    refusal: Option<String>,
) -> ExtractError {
    match provider_side {
        Some(e) => e,
        None => match refusal {
            Some(m) => ExtractError::InvalidRequest(m),
            None => ExtractError::NoHealthyKey("No healthy extract key".into()),
        },
    }
}

/// Extract-chain (url chain) error → mode mapping.
///
/// Same classes as the shared `verdict_for`, EXCEPT transport (`Http`) errors
/// are retryable here: connection-refused/transport failures retry the same
/// account (pinned: 3 attempts / 2 retries on `127.0.0.1:9`), while
/// `Unsupported`/`Unextractable` return immediately (report decides the hold
/// finishing only — the retry loop below applies the mode).
///
/// `402` is checked BEFORE the exhausted-status table (same split as
/// `search/run_provider.rs::report_mode`): `is_exhausted_status` folds out-of-money
/// and rate-limited together, but only `402` may zero `credits_remaining` — the
/// preserving exhausted write leaves a NULL-credit exa/xAI key eligible forever,
/// re-serving `402` every lap.
fn report_mode(provider: &str, e: &ProviderError) -> ReportMode {
    match e {
        ProviderError::Upstream { status: 402, .. } => ReportMode::PaymentRequired,
        ProviderError::Upstream { status, .. } if is_exhausted_status(provider, *status) => {
            ReportMode::Exhausted
        }
        ProviderError::Upstream { status, body, .. }
            if is_account_banned(provider, *status, body) =>
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
        // `Unsupported` is BY CONTRACT a client-side refusal, never a vendor
        // response (providers/src/lib.rs: "consumers must never treat this as a
        // vendor response, only as a client-side unsupported request"), so it
        // maps to the same class the batch mapper already used. Surfacing it is
        // gated by `surfaced_extract_err` below: the chain keeps walking past a
        // refusal, and the 400 only reaches the caller when no leg failed
        // provider-side. The message text is unchanged, so the research social
        // leg's `social_error` string is byte-identical.
        ProviderError::Unsupported {
            provider,
            action,
            detail,
        } => ExtractError::InvalidRequest(format!("{provider} {action} unsupported: {detail}")),
        // `402` gets its own copy — "rate-limited, try again shortly" is the one
        // message that sends an agent into a retry loop against a dead balance.
        // The kind stays `Provider`/502 `retryable:true`: another key in the pool
        // may genuinely have credit; only this account is out of money (its
        // `PaymentRequired` report zeroes it, so the retry lands funded).
        ProviderError::Upstream { status: 402, .. } => {
            ExtractError::Provider(format!("{provider} is out of credits (upstream 402)"))
        }
        ProviderError::Upstream { status, .. } if is_exhausted_status(provider, *status) => {
            ExtractError::Provider(format!(
                "{provider} rate-limited (upstream {status}); try again shortly"
            ))
        }
        // Agent-facing messages carry NO vendor response text — even a
        // snippet can contain alarming wording ("key banned", account ids)
        // that derails agent execution. The verbatim body lives only in the
        // server WARN log (`reason=upstream_error` / `account_banned`).
        ProviderError::Upstream { status, body, .. }
            if is_account_banned(provider, *status, body) =>
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
                            provider = provider,
                            status = *status,
                            body = %body,
                            reason = "account_banned",
                            disposition =
                                if provider == serpotter_providers::SVC_FIRECRAWL {
                                    "deleted"
                                } else {
                                    "suspended"
                                },
                            "vendor-banned key removed from pool"
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
            // C3a-fix (P1 pre-refresh gap): the start call alone can burn the
            // full 60 s HTTP timeout, and the first status call would push
            // the unrefreshed window to ~120 s > the 90 s hold TTL — the
            // reclaim-then-late-clobber race. Refresh right after job
            // creation so every segment between refreshes is ONE <=60 s call
            // (+ the 2 s tick), always under the TTL.
            key_refresh.refresh().await;
            if let Some(ph) = &proxy_refresh {
                ph.refresh().await;
            }
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
    mut req: crate::dto::ExtractRequest,
) -> Result<ProductOutcome<crate::dto::ExtractResponse>, ProductOutcome<ExtractError>> {
    // The extract surface's single canonicalization point for client paths
    // (the twin of `search_inner`'s `body.canonicalize()`): `provider` is
    // compared VERBATIM everywhere below — `chain_for`'s arms, the batch
    // backend's `preferred == Some(exa)` pick — and feeds both extract cache
    // keys, so the fold MUST precede the first of those reads: with the
    // boundaries now lenient on spelling, an un-canonicalized `" Tavily "`
    // past this point is a misroute, not a typo. Member-gated like core's
    // `fold_member`: a non-member stays verbatim so the `Some(other)`
    // refusal still quotes the client's bytes.
    // `format` is deliberately NOT folded: it is not on the contract's
    // covered-knob list, the MCP boundary matches it exactly, and folding it
    // here would widen REST alone — a surface divergence this wave does not
    // authorize. `urls`/`question`/`prompt` carry page content, not
    // closed-set knobs (vendor-visible; no Covers rule).
    req.provider = req.provider.as_deref().map(fold_extract_provider);
    // Post-fold filters, so `" Auto "`/`"auto"` and the empty string all mean
    // "unset → chain default", matching what both boundaries already accept.
    let preferred = req
        .provider
        .as_deref()
        .filter(|p| *p != "auto" && !p.is_empty());
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
        ExaClient, FirecrawlClient, ProviderError, ProviderRegistry, TavilyClient, XaiClient,
    };

    use crate::error::ExtractError;
    use crate::lease::ReportMode;
    use crate::meta::{ProgressEvent, ProgressSink};
    use crate::ProductCtx;

    use super::{map_batch_provider_error, map_provider_error, report_mode, surfaced_extract_err};

    fn upstream(provider: &str, status: u16, body: &str) -> ProviderError {
        ProviderError::Upstream {
            provider: provider.to_string(),
            status,
            body: body.to_string(),
        }
    }

    fn refusal(provider: &str, detail: &str) -> ProviderError {
        ProviderError::Unsupported {
            provider: provider.to_string(),
            action: "extract",
            detail: detail.to_string(),
        }
    }

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

    /// Pool hygiene at the verdict boundary (same split the search ladder uses):
    /// `402` must outrank the exhausted-status table so the key report zeroes
    /// credits, while `429`/`432`/`433` stay `Exhausted` and 401/403 keep their
    /// arms. `is_exhausted_status("exa", 402)` is `true`, so relying on it alone
    /// is the drift this test defends.
    #[test]
    fn report_mode_separates_payment_from_rate_limit() {
        assert_eq!(
            report_mode("exa", &upstream("exa", 402, "NO_MORE_CREDITS")),
            ReportMode::PaymentRequired,
            "402 must win over the exhausted table"
        );
        assert_eq!(
            report_mode("firecrawl", &upstream("firecrawl", 402, "credits")),
            ReportMode::PaymentRequired
        );
        assert_eq!(
            report_mode("exa", &upstream("exa", 429, "")),
            ReportMode::Exhausted,
            "a rate limit must not zero a balance"
        );
        assert_eq!(
            report_mode("tavily", &upstream("tavily", 433, "")),
            ReportMode::Exhausted
        );
        assert_eq!(
            report_mode("exa", &upstream("exa", 401, "Unauthorized")),
            ReportMode::AuthFailure
        );
        // A refusal releases the hold — never fail@3 a key for a parameter.
        assert_eq!(
            report_mode("tavily", &refusal("tavily", "bad format")),
            ReportMode::Failure
        );
    }

    /// The single-extract mapper now reports `Unsupported` as
    /// `ExtractError::InvalidRequest`, matching the variant's own contract
    /// ("consumers must never treat this as a vendor response, only as a
    /// client-side unsupported request") and the batch mapper. Whether the
    /// caller SEES the 400 is `surfaced_extract_err`'s all-or-nothing rule. The
    /// text is byte-identical to the old class, which matters because
    /// `research_inner` embeds `map_provider_error(SVC_XAI, &e).to_string()` in
    /// the soft-fail `social_error` string — pinned here so that wire cannot
    /// shift silently. A vendor 400 stays a provider error (our payload bugs),
    /// and a 402 keeps the provider class while saying "out of credits".
    #[test]
    fn single_extract_maps_refusal_to_invalid_request_and_400_402_to_provider() {
        match map_provider_error(
            "tavily",
            &refusal("tavily", "format=question needs firecrawl"),
        ) {
            ExtractError::InvalidRequest(m) => {
                assert_eq!(
                    m,
                    "tavily extract unsupported: format=question needs firecrawl"
                )
            }
            other => panic!("a local refusal is a client-shape error, got {other:?}"),
        }
        // Research social-leg pin: the exact string that lands in `social_error`.
        assert_eq!(
            map_provider_error(
                "xai",
                &refusal(
                    "xai",
                    "include_domains are not supported on the social path"
                ),
            )
            .to_string(),
            "xai extract unsupported: include_domains are not supported on the social path"
        );
        assert!(matches!(
            map_provider_error(
                "firecrawl",
                &upstream("firecrawl", 400, r#"{"error":"maxAge is not supported"}"#)
            ),
            ExtractError::Provider(m) if m == "firecrawl upstream error (status 400)"
        ));
        // 402 changes the KEY report (`PaymentRequired`) and the copy, never the
        // caller-facing class: another key may have credit, so the REQUEST is
        // still retryable.
        assert!(matches!(
            map_provider_error("exa", &upstream("exa", 402, "NO_MORE_CREDITS")),
            ExtractError::Provider(m) if m == "exa is out of credits (upstream 402)"
        ));
    }

    /// The extract chain's aggregation rule, same as the search chain's: a
    /// provider-side failure outranks a refusal (weaker evidence about the
    /// request than an outage is), so only an all-refused chain surfaces the
    /// caller's 400 — and an empty chain keeps today's `NoHealthyKey`.
    #[test]
    fn extract_chain_aggregation_prefers_provider_side_failure() {
        const CAPS: &str = "tavily extract unsupported: caps";
        let outage = || ExtractError::Provider("exa upstream error (status 503)".into());
        let missing = || ExtractError::NoHealthyKey("No healthy firecrawl key".into());
        // A provider-side failure outranks a refusal, whichever leg came last.
        assert!(matches!(
            surfaced_extract_err(Some(outage()), Some(CAPS.into())),
            ExtractError::Provider(m) if m == "exa upstream error (status 503)"
        ));
        // A lease-side failure counts as provider-side too (it is about the
        // pool's reachability, not the caller's parameters).
        assert!(matches!(
            surfaced_extract_err(Some(missing()), Some(CAPS.into())),
            ExtractError::NoHealthyKey(_)
        ));
        // Every leg refused → the caller's own mistake is the honest answer.
        assert!(matches!(
            surfaced_extract_err(None, Some(CAPS.into())),
            ExtractError::InvalidRequest(m) if m == CAPS
        ));
        // Nothing ran: today's default message.
        assert!(matches!(
            surfaced_extract_err(None, None),
            ExtractError::NoHealthyKey(m) if m == "No healthy extract key"
        ));
    }

    /// The batch mapper's `Unsupported → InvalidRequest` stays: batch has no
    /// fallback chain to hop, so there the refusal IS the terminal local gate.
    /// Its 400 handling is unchanged, which is the symmetry the search path
    /// keeps (`Upstream` → `Provider`).
    #[test]
    fn batch_mapper_keeps_local_gate_400_and_vendor_400_as_provider() {
        match map_batch_provider_error(
            "firecrawl",
            &ProviderError::Unsupported {
                provider: "firecrawl".into(),
                action: "extract_batch",
                detail: "batch extract unsupported".into(),
            },
        ) {
            ExtractError::InvalidRequest(m) => {
                assert_eq!(
                    m,
                    "firecrawl extract_batch unsupported: batch extract unsupported"
                )
            }
            other => panic!("the batch gate is a client 400, got {other:?}"),
        }
        assert!(matches!(
            map_batch_provider_error(
                "tavily",
                &upstream("tavily", 400, r#"{"error":"bad url"}"#)
            ),
            ExtractError::Provider(m) if m == "tavily upstream error (status 400)"
        ));
    }

    /// The ordering guard for the canonicalization wave: the boundaries now
    /// ACCEPT `" Tavily "`/`"Exa"`-class spellings, which only stays safe
    /// because `extract_dispatch` folds `provider` BEFORE `chain_for`/the
    /// batch pick read it. Driving the real entry point (not the pure fold)
    /// is the point: a `canonicalize()` moved below the comparisons would
    /// still pass a unit test on the fold itself. Every provider points at
    /// 127.0.0.1:9, so the call fails — what matters is WHICH leg is dialed
    /// FIRST: the preferred provider's ladder Attempt. Unfolded, a
    /// `" Tavily "` instead dies in `chain_for`'s `Some(other)` arm as
    /// "unknown extract provider" with ZERO Attempt events — the "no leg was
    /// dialed" panic below is what catches that regression.
    #[tokio::test]
    async fn extract_dispatch_dials_the_spelled_provider_first() {
        use crate::dto::ExtractRequest;
        for (spelling, expected_head) in [
            ("tavily", "tavily"),
            (" Tavily ", "tavily"),
            ("TAVILY", "tavily"),
            ("Exa", "exa"),
            (" exa", "exa"),
            ("firecrawl", "firecrawl"),
            (" Firecrawl", "firecrawl"),
        ] {
            let db = test_db().await;
            // Every chain head holds a key, so the FIRST leg always reaches
            // the ladder and emits its Attempt before any fallback.
            for svc in ["tavily", "firecrawl", "exa"] {
                db.insert_api_key(svc, &format!("{svc}-spelling-chain"))
                    .await
                    .unwrap();
            }
            let sink = VecSink::default();
            let ctx = ctx_for(db, sink.clone());
            let req = ExtractRequest {
                url: "https://example.com".into(),
                provider: Some(spelling.into()),
                prompt: None,
                schema: None,
                urls: None,
                format: None,
                question: None,
                output_schema: None,
            };
            let _ = super::extract_dispatch(&ctx, req).await;
            let first = sink
                .0
                .lock()
                .unwrap()
                .iter()
                .find_map(|e| match e {
                    ProgressEvent::Attempt { service, .. } => Some(service.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("provider={spelling:?}: no leg was dialed"));
            assert_eq!(
                first, expected_head,
                "provider={spelling:?} must head the chain with {expected_head:?}, the \
                 dispatch entry folded it before the comparisons read it"
            );
        }
    }
}
