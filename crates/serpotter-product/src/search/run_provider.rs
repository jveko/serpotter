//! Single-provider attempt loop (key/proxy dual-pool matrix) on
//! [`crate::lease::with_key_proxy`].

use serpotter_core::SearchQuery;
use serpotter_providers::{ProviderError, ProviderResult, ProviderSearchParams, SVC_XAI};

use crate::error::SearchExecError;
use crate::lease::{with_key_proxy, LeaseError, ReportMode};
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::ProductCtx;

use super::{is_account_banned, is_exhausted_status};
use crate::lease::verdict_for;

/// Search-path retry budget (unchanged; pinned by tests).
const MAX_ATTEMPTS: u32 = 3;

// The error -> mode mapping is `lease::verdict_for` (ONE classifier for every
// leg — search, extract, research — so a verdict can never differ between two
// call sites that copy-pasted the same match). Its doc on the definition owns
// the rationale; it checks `402` BEFORE the exhausted-status table because
// `402` is the only upstream fact that changes how the KEY is reported.
/// Per-class failure message strings (pinned by api/product tests).
fn map_provider_error(provider: &str, e: &ProviderError) -> SearchExecError {
    match e {
        ProviderError::Unextractable { message, .. } => {
            SearchExecError::Provider(format!("{provider} unextractable: {message}"))
        }
        // A local refusal IS the client's parameter shape (our own text, never a
        // vendor body) — the same class extract's mappers report. It only reaches
        // the caller as a 400 once `run_chain` / `leg_aggregate_err` prove that
        // every leg that failed refused: a vendor-specific knob (`country` is
        // tavily-only) still hops to a vendor that ignores it, and any
        // provider-side leg failure keeps precedence. Message shape is pinned by
        // tests.
        ProviderError::Unsupported {
            provider,
            action,
            detail,
        } => SearchExecError::InvalidRequest(format!("{provider} {action} unsupported: {detail}")),
        // `402` gets its own copy: "rate-limited, try again shortly" is the one
        // message that would send an agent into a retry loop against a dead
        // balance. The kind stays `Provider`/502 and `retryable:true` because
        // THIS request may still be served by another key in the pool — only the
        // refusing account is out of money (the `PaymentRequired` report zeroes
        // it, so the retry lands on a funded key).
        ProviderError::Upstream { status: 402, .. } => {
            SearchExecError::Provider(format!("{provider} is out of credits (upstream 402)"))
        }
        ProviderError::Upstream { status, .. } if is_exhausted_status(provider, *status) => {
            SearchExecError::Provider(format!(
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
            SearchExecError::Provider(format!("{provider} temporarily unavailable"))
        }
        // No `status: 400` arm on purpose. A vendor 400 has repeatedly been OUR
        // payload bug (the Firecrawl `maxAge` wave: 69 occurrences Aug 27–30),
        // and `retryable:false` would also short-circuit the fallback that
        // rescued those callers. A client 400 is earned by a LOCAL refusal or by
        // the pre-lease shape gate in `search_inner` — never by reading a
        // vendor-side fact — and the verbatim body stays in the WARN log below.
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
/// Shared with the research/social retry ladder (crate-internal only).
pub(crate) fn retry_backoff_ms(attempt: u32) -> u64 {
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
            |e| verdict_for(provider, e),
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
                        // B9 wiring: tavily-only surface — other providers
                        // ignore these.
                        include_images: body.include_images,
                        // Same strip discipline as the hybrid x leg's domain
                        // filters (`execute::execute_hybrid`): a leg running
                        // ALONGSIDE a web leg must not carry web-only intent the
                        // xAI dialect refuses — xAI answers with no page
                        // content, so `include_raw_content` there would refuse
                        // the whole leg (400 via `leg_aggregate_err`) and throw
                        // away a valid hybrid request. Keyed on the ROUTING
                        // decision, not the leg-local `sources` (the x leg is
                        // handed `["x"]`); blend never carries an xAI leg.
                        // xAI-direct / x-only requests keep the flag and still
                        // get the honest refusal.
                        include_raw_content: body.include_raw_content
                            && !(provider == SVC_XAI && decision.hybrid),
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
                let mode = verdict_for(provider, &e);
                last_err = map_provider_error(provider, &e);
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
                            verdict = crate::lease::outcome_label(mode, Some(*status)),
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
                    tracing::info!(
                        service = provider,
                        attempt,
                        reason = %last_err,
                        "provider retry"
                    );
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
    use serpotter_providers::ProviderError;

    use crate::error::SearchExecError;

    use super::{map_provider_error, retry_backoff_ms};

    fn upstream(provider: &str, status: u16, body: &str) -> ProviderError {
        ProviderError::Upstream {
            provider: provider.to_string(),
            status,
            body: body.to_string(),
        }
    }

    /// A local refusal maps to `InvalidRequest`, but see the aggregation in
    /// `run_chain` / `execute::leg_aggregate_err`: the class only reaches the
    /// caller when EVERY failed leg refused, so a tavily-only knob still gets
    /// served by the next vendor. The message shape is the contract CoreArgs'
    /// provider guards and the api mapping tests pin from the other side.
    #[test]
    fn map_unsupported_is_invalid_request() {
        match map_provider_error(
            "tavily",
            &ProviderError::Unsupported {
                provider: "tavily".into(),
                action: "search",
                detail: "country must be a full country name".into(),
            },
        ) {
            SearchExecError::InvalidRequest(m) => assert_eq!(
                m,
                "tavily search unsupported: country must be a full country name"
            ),
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
        // The real prod string (xAI domain cap, 2026-09-09).
        match map_provider_error(
            "exa",
            &ProviderError::Unsupported {
                provider: "exa".into(),
                action: "search",
                detail: "exa search accepts at most 20 includedDomains".into(),
            },
        ) {
            SearchExecError::InvalidRequest(m) => assert_eq!(
                m,
                "exa search unsupported: \
                 exa search accepts at most 20 includedDomains"
            ),
            other => panic!("expected InvalidRequest, got {other:?}"),
        }
    }

    /// A vendor 400 is NOT the client's error: our own history says it is
    /// usually OUR payload bug (the Firecrawl `maxAge` wave — 69 occurrences
    /// Aug 27–30). It therefore keeps `Provider` → 502 `ProviderError`
    /// `retryable:true`, and the fallback chain stays available. Do not add a
    /// blanket `status: 400 → InvalidRequest` arm: none of the three existing
    /// mappers (search, single-extract, batch-extract) classifies upstream 400
    /// as a client error, and fixing outbound shapes is the providers' job.
    #[test]
    fn map_upstream_400_stays_provider_error() {
        match map_provider_error(
            "tavily",
            &upstream("tavily", 400, r#"{"detail":{"error":"Invalid country."}}"#),
        ) {
            SearchExecError::Provider(m) => {
                assert_eq!(m, "tavily upstream error (status 400)");
                // Vendor text never reaches the agent — it lives in the WARN log.
                assert!(!m.contains("Invalid country"), "vendor text leaked: {m}");
            }
            other => panic!("an upstream 400 must stay a provider error, got {other:?}"),
        }
        // Ban wording inside a 400 body is not an account problem either (the
        // banned arm gates on 401/403): still the plain upstream error.
        assert!(matches!(
            map_provider_error("tavily", &upstream("tavily", 400, "account has been banned")),
            SearchExecError::Provider(m) if m == "tavily upstream error (status 400)"
        ));
    }

    /// The boundary that matters most: genuine provider-side failures keep
    /// their 502 `ProviderError` class (and therefore `retryable:true` plus
    /// the retry/fallback ladder) on every status the guards do not claim.
    #[test]
    fn map_provider_side_statuses_stay_provider_error() {
        // 401: auth failure on THIS key — retried/fell back, never client's fault.
        assert!(matches!(
            map_provider_error("exa", &upstream("exa", 401, "Unauthorized")),
            SearchExecError::Provider(m) if m == "exa upstream error (status 401)"
        ));
        // 403 with account-ban wording: the banned arm, not the generic arm.
        assert!(matches!(
            map_provider_error("exa", &upstream("exa", 403, "This account has been banned.")),
            SearchExecError::Provider(m) if m == "exa temporarily unavailable"
        ));
        // Tavily's exact deactivation body on 403 → same.
        let deactivate = "The account associated with this API key has been deactivated.";
        assert!(matches!(
            map_provider_error("tavily", &upstream("tavily", 403, deactivate)),
            SearchExecError::Provider(m) if m == "tavily temporarily unavailable"
        ));
        // 402 keeps the Provider/502 retryable class (another key may have
        // credit) but gets its own honest copy — never "rate-limited, try again
        // shortly", which invites a retry against a dead balance. 429/432/433
        // keep the rate-limit wording. The KEY-side split is `PaymentRequired`.
        assert!(matches!(
            map_provider_error("firecrawl", &upstream("firecrawl", 402, "no credits")),
            SearchExecError::Provider(m) if m == "firecrawl is out of credits (upstream 402)"
        ));
        assert!(matches!(
            map_provider_error("exa", &upstream("exa", 402, "NO_MORE_CREDITS")),
            SearchExecError::Provider(m) if m == "exa is out of credits (upstream 402)"
        ));
        assert!(matches!(
            map_provider_error("exa", &upstream("exa", 429, "")),
            SearchExecError::Provider(m) if m.starts_with("exa rate-limited (upstream 429)")
        ));
        // 5xx is an outage.
        assert!(matches!(
            map_provider_error("exa", &upstream("exa", 503, "unavailable")),
            SearchExecError::Provider(m) if m == "exa upstream error (status 503)"
        ));
    }

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

    /// `Unextractable` (an empty vendor page) is a provider-side outcome, not
    /// a client parameter mistake — it keeps the 502 `ProviderError` class.
    #[test]
    fn map_unextractable_stays_provider_error() {
        assert!(matches!(
            map_provider_error(
                "exa",
                &ProviderError::Unextractable {
                    provider: "exa".into(),
                    message: "empty page".into(),
                },
            ),
            SearchExecError::Provider(m) if m == "exa unextractable: empty page"
        ));
    }
}
