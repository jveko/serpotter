//! Dual-pool lease combinator: acquire key → (optional) proxy → client_for →
//! run ONE provider call → finish holds per the verdict → note_attempt.
//!
//! Replaces the copy-pasted "acquire key → `KeyHold` → acquire proxy →
//! `ProxyHold` → `client_for` → call → finish/note_attempt" ladders across
//! the search retry loop, deep search, extract legs, and research legs with
//! ONE combinator whose behavior is pinned by the unit tests below.
//!
//! # Report modes (B9)
//!
//! [`verdict_for`] maps a provider error to the mode that drives hold
//! finishing — the single classifier shared by the search and extract retry
//! ladders. Legs that want different semantics pass their own `report`
//! closure. The four SINGLE-ATTEMPT extract paths (`extract_structured`,
//! `batch_via`, `extract_question_dispatch`, `extract_highlights_dispatch`)
//! and the tavily-research leg run `verdict_for` like every other leg, with
//! ONE remap: `Banned → AuthFailure`
//! (`extract_url::structured_leg_verdict`). Their bodies are the least
//! reliable ban signal in the system — they carry vendor-produced text — and
//! the firecrawl ban tier DELETEs the row, so they demote instead. A `402`
//! therefore reaches `PaymentRequired` on these legs too, and a `401`
//! accumulates toward `fail@3` instead of being released.
//!
//! # Hold finishing per verdict
//!
//! | verdict     | key              | proxy            |
//! |-------------|------------------|------------------|
//! | Ok          | `finish_success` | `finish_success` |
//! | Failure     | `finish_release` | `finish_release` |
//! | Exhausted   | `finish_exhausted`| `finish_release` |
//! | PaymentRequired | `finish_payment_required` | `finish_release` |
//! | AuthFailure | `finish_failure` | `finish_release` |
//! | Banned      | `finish_banned` (firecrawl, hard-delete) / `finish_suspended` (others, permanently out of rotation) | `finish_release` |
//! | Retryable   | `finish_release` | `finish_release` |
//!
//! # Emission ownership
//!
//! `with_key_proxy` emits [`ProgressEvent::Attempt`], owns the
//! `provider_attempt` info span (service/key_id/node_id/attempt/outcome),
//! builds the http `Client` via `ProviderRegistry::client_for`, records
//! `meta.note_attempt`, and finishes every hold. Retry/fallback events are
//! the CALLER's job (the run_provider retry loop emits
//! [`ProgressEvent::Retry`] when it decides to retry a `Retryable`/`Banned`/
//! `AuthFailure`/Http verdict).
//!
//! A `client_for` failure (the leased node's proxy URL does not build) is not
//! a verdict — no provider call ran — but it IS a node-configuration fact, so
//! the ladder blames the NODE (`consecutive_fails`++, disable at 3) and only
//! releases the key. Releasing the node instead made it a permanent
//! least-inflight magnet.

use std::future::Future;
use std::sync::Arc;
use tracing::Instrument as _;

use serpotter_keypool::KeyPoolError;
use serpotter_providers::{is_tunnel_error, ProviderError, SVC_FIRECRAWL};

use crate::hold::{KeyHold, KeyRefresh, ProxyHold, ProxyRefresh};
use crate::meta::{ExecMeta, ProgressEvent};
use crate::search::{is_account_banned, is_exhausted_status, is_payment_required_status};
use crate::ProductCtx;

/// How a provider call ended; drives hold finishing (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportMode {
    Ok,
    Failure,
    Exhausted,
    /// Upstream `402` — the account is out of money. Distinct from
    /// [`ReportMode::Exhausted`] because the key report differs: exhausted
    /// keeps `NULL` credits `NULL` (a `429` must not sink a healthy key),
    /// while payment-required zeroes them so the row falls into the
    /// exhausted-last tier instead of re-serving `402` forever at
    /// unknown-credit weight.
    PaymentRequired,
    AuthFailure,
    Banned,
    Retryable,
}

/// Acquire-side lease failures (before any provider call runs). The API
/// shells map these to their own error types via `acquire_err`.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// No active key for the service ("No healthy {s} key").
    #[error("{0}")]
    NoHealthyKey(String),
    /// Active keys exist but all were at `max_inflight` until acquire deadline.
    #[error("{0}")]
    KeyBusy(String),
    /// Outbound required but no healthy node ("REQUIRE_OUTBOUND_PROXY").
    #[error("{0}")]
    NoHealthyNode(String),
    #[error(transparent)]
    Db(serpotter_db::DbError),
}

/// Fallback cooldown when the vendor sent no usable `Retry-After` on a 429.
pub const DEFAULT_COOLDOWN_SECS: i64 = 60;
/// Hard ceiling on a vendor-suggested cooldown: a key parked for an hour on a
/// vendor's word would sit at the back of the rotation far longer than the
/// vendor asked. The acquire path DEMOTES cooling keys, it never filters them,
/// so this bounds queue time, not availability.
pub const MAX_COOLDOWN_SECS: i64 = 3600;

/// Cooldown to stamp for an exhausted report: the vendor's `Retry-After`
/// (clamped to [`MAX_COOLDOWN_SECS`]) when it sent a parseable one, else
/// [`DEFAULT_COOLDOWN_SECS`]. The `Ok` payload is irrelevant to the cooldown;
/// the generic keeps call sites free of a throwaway binding.
pub fn cooldown_secs_for<T>(result: &Result<T, ProviderError>) -> i64 {
    let advertised = match result {
        Err(ProviderError::Upstream {
            retry_after_secs, ..
        }) => *retry_after_secs,
        _ => None,
    };
    // Clamp in the UNSIGNED domain BEFORE the cast. `s as i64` would wrap a
    // `Retry-After` at or above 2^63 (parse_retry_after accepts `u64::MAX`)
    // to a negative i64, `.min(3600)` would pass that negative straight
    // through, and the db's `.max(0)` would then stamp `'+0 seconds'` — a
    // silently DROPPED cooldown, the exact inverse of the honest-absence
    // rule this whole path exists to honour.
    advertised
        .map(|s| s.min(MAX_COOLDOWN_SECS as u64) as i64)
        .unwrap_or(DEFAULT_COOLDOWN_SECS)
}

/// Default error → mode mapping (B9 semantics, shared by all search legs):
/// - Upstream 402 → [`ReportMode::PaymentRequired`] (checked first: 402 is
///   also an "exhausted" status per [`is_exhausted_status`], but the key
///   report must differ — see the variant doc)
/// - Upstream status that is exhausted for `provider` → [`ReportMode::Exhausted`]
/// - Firecrawl permanent ban (status + body markers) → [`ReportMode::Banned`]
/// - Upstream 401/403 → [`ReportMode::AuthFailure`]
/// - Upstream 429 or 500..600 → [`ReportMode::Retryable`]
/// - Transport (`Http`) errors → [`ReportMode::Retryable`] (same account
///   retry; tunnel failures blame the leased node in the ladder's finish)
/// - everything else (Unextractable, Unsupported, other statuses) → [`ReportMode::Failure`]
pub fn verdict_for(provider: &str, e: &ProviderError) -> ReportMode {
    match e {
        ProviderError::Upstream { status, body, .. } => {
            if is_payment_required_status(*status) {
                ReportMode::PaymentRequired
            } else if is_exhausted_status(provider, *status) {
                ReportMode::Exhausted
            } else if is_account_banned(provider, *status, body) {
                ReportMode::Banned
            } else if *status == 401 || *status == 403 {
                ReportMode::AuthFailure
            } else if *status == 429 || (500..600).contains(status) {
                ReportMode::Retryable
            } else {
                ReportMode::Failure
            }
        }
        // Transport errors retry (the ladder blames a leased node on tunnel
        // failures); Unextractable/Unsupported are never upstream statuses.
        ProviderError::Http(_) => ReportMode::Retryable,
        _ => ReportMode::Failure,
    }
}

/// Closed outcome label for the observability counter (plan §label set).
/// `AuthFailure` splits on the upstream status: 403 → forbidden, else auth_invalid.
pub fn outcome_label(mode: ReportMode, upstream_status: Option<u16>) -> &'static str {
    match mode {
        ReportMode::Ok => "ok",
        ReportMode::PaymentRequired => "payment_required",
        ReportMode::Exhausted => "rate_limited",
        ReportMode::AuthFailure if upstream_status == Some(403) => "forbidden",
        ReportMode::AuthFailure => "auth_invalid",
        ReportMode::Banned => "banned",
        ReportMode::Retryable => "retryable",
        ReportMode::Failure => "failure",
    }
}

/// Run one provider call under the dual-pool ladder.
///
/// Owns: the [`ProgressEvent::Attempt`] emission, the `provider_attempt`
/// info span (service/key_id/node_id/attempt/outcome — `outcome` recorded
/// after the call from the verdict), the http `Client` via
/// `ctx.providers.client_for(proxy_url)`, the `meta.note_attempt` record,
/// and every hold finish per the module-doc table.
///
/// The call closure receives the leased `api_key`, `proxy_url`, the http
/// `Client`, plus OWNED refresh handles ([`KeyRefresh`], optional
/// [`ProxyRefresh`]) so LONG-POLL calls (structured extract, tavily research)
/// can re-stamp their leases mid-call — the ladder still finishes the real
/// holds per the verdict after the call returns, exactly as before.
///
/// - `direct=true` skips the outbound acquire entirely (xAI).
/// - Acquire-side failures (NoHealthyKey / KeyBusy / NoHealthyNode / Db) map
///   through `acquire_err(LeaseError)` and return `Err(E)`.
/// - `client_for` failures blame the NODE (its proxy URL did not build) and
///   release the key, surfacing as `Ok(Err(e))` with the exact `ProviderError`
///   `client_for` returned; the KEY verdict stays
///   [`ReportMode::Failure`] because nothing about the account is at fault.
///
/// Returns `Ok(Ok(t))` on success, `Ok(Err(e))` after the provider call
/// (holds already finished per `report(e)`), `Err(E)` on acquire failure.
///
/// Args are OWNED (`String`/`Option<String>`/`Client` clones — negligible at
/// this scale) so the call closure can `async move` them without lifetime
/// pain; the two hold refs are `Copy` and safe to move into the block too.
#[allow(clippy::too_many_arguments)]
pub async fn with_key_proxy<T, E, A, R, C, Fut>(
    ctx: &ProductCtx,
    service: &str,
    direct: bool,
    attempt: u32,
    max_attempts: u32,
    meta: &mut ExecMeta,
    acquire_err: A,
    report: R,
    call: C,
) -> Result<Result<T, ProviderError>, E>
where
    A: Fn(LeaseError) -> E,
    R: Fn(&ProviderError) -> ReportMode,
    C: FnOnce(String, Option<String>, reqwest::Client, KeyRefresh, Option<ProxyRefresh>) -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    ctx.emit(&ProgressEvent::Attempt {
        service: service.to_string(),
        attempt,
        max: max_attempts,
    });

    let lease = match ctx.keys.acquire(service).await {
        Ok(k) => k,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(acquire_err(LeaseError::NoHealthyKey(format!(
                "No healthy {s} key"
            ))));
        }
        Err(KeyPoolError::AcquireTimeout(s)) => {
            return Err(acquire_err(LeaseError::KeyBusy(format!(
                "All {s} keys busy (acquire timeout)"
            ))));
        }
        Err(KeyPoolError::Db(e)) => {
            return Err(acquire_err(LeaseError::Db(e)));
        }
    };
    let mut key_hold = KeyHold::new(Arc::clone(&ctx.keys), lease.identity());
    let key_id = key_hold.key_id();

    // xAI always dials direct; web providers acquire (node / direct). When
    // `direct=true` the outbound pool is never touched, not even to fail on
    // `require_proxy` — that guard applies to proxied legs only.
    let proxy = if direct {
        None
    } else {
        match ctx.outbound.acquire().await {
            Ok(None) if ctx.outbound.require_proxy() => {
                key_hold.finish_release().await;
                return Err(acquire_err(LeaseError::NoHealthyNode(
                    "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)".into(),
                )));
            }
            Ok(p) => p,
            Err(serpotter_outbound::ProxyPoolError::Db(e)) => {
                // Explicit release before return (Drop spawn is only the safety net).
                key_hold.finish_release().await;
                return Err(acquire_err(LeaseError::Db(e)));
            }
        }
    };
    let mut proxy_hold = proxy
        .as_ref()
        .map(|p| ProxyHold::new(Arc::clone(&ctx.outbound), p.clone()));
    let node_id = proxy_hold.as_ref().map(|h| h.node_id());
    let proxy_url = proxy.as_ref().map(|p| p.url.clone());

    // F10 attribution: publish the attempt BEFORE the call so a deadline that
    // elapses mid-call still reports the vendor/key/node it was on.
    meta.note_attempt_pending(service, key_id, node_id);
    ctx.observe_meta(meta);

    let span = tracing::info_span!(
        "provider_attempt",
        service = service,
        key_id = key_id,
        node_id = ?node_id,
        attempt = attempt,
        outcome = tracing::field::Empty,
    );
    // Everything from the http-client build through hold finishing runs in
    // ONE future instrumented with `provider_attempt`: events emitted by
    // `finish_*`/release paths stay parented to the attempt, and unlike an
    // `enter()` guard held across `.await`s this never leaks the span onto
    // unrelated tasks polled on the same worker (tracing async rule).
    let (result, verdict) = async move {
        // Build the http client for this attempt's egress (None → direct client).
        // A proxied URL the builder REJECTS is a node-configuration fact, not a
        // vendor blip: the leased node's own `Proxy::all` string never parsed, so
        // the node is blamed (`consecutive_fails`++ → disable at 3) exactly like a
        // tunnel failure. Releasing it instead left the node at inflight 0 and
        // `inflight ASC, id ASC` picking it for EVERY proxied leg — a permanent
        // least-inflight magnet that killed all proxied traffic until an operator
        // noticed. The key stays release-only: our own config is what broke.
        let client = match ctx.providers.client_for(proxy_url.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_failure(Some(&crate::hold::safe_node_error(&e.to_string())))
                        .await;
                }
                meta.note_attempt(service, key_id, node_id, false);
                // No upstream status exists for a client-construction failure:
                // the vendor was never dialed.
                meta.note_outcome(service, key_id, "failure", None);
                ctx.observe_meta(meta);
                return (Err(e), ReportMode::Failure);
            }
        };

        let key_refresh = KeyRefresh::new(Arc::clone(&ctx.keys), lease.identity());
        let proxy_refresh = proxy
            .as_ref()
            .map(|p| ProxyRefresh::new(Arc::clone(&ctx.outbound), p.clone()));
        let result = call(lease.key, proxy_url, client, key_refresh, proxy_refresh).await;
        let verdict = match &result {
            Ok(_) => ReportMode::Ok,
            Err(e) => report(e),
        };

        match verdict {
            ReportMode::Ok => {
                key_hold.finish_success().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_success().await;
                }
            }
            ReportMode::Failure => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
            }
            ReportMode::Exhausted => {
                let t = key_hold
                    .finish_exhausted(service, cooldown_secs_for(&result))
                    .await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                meta.note_transition(service, key_id, t);
            }
            ReportMode::PaymentRequired => {
                let t = key_hold.finish_payment_required(service).await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                meta.note_transition(service, key_id, t);
            }
            ReportMode::AuthFailure => {
                let t = key_hold.finish_failure(service).await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                meta.note_transition(service, key_id, t);
            }
            ReportMode::Banned => {
                // Two tiers: firecrawl's proven signature hard-deletes the row;
                // other vendors' likely-tier matches only disable (active=0) and
                // stamp `disabled_reason = 'vendor_suspended'`, which the 24h
                // re-enable cron now skips — a vendor-deactivated account must not
                // come back on a timer and re-401 forever (schema 18).
                let t = if service == SVC_FIRECRAWL {
                    key_hold.finish_banned().await
                } else {
                    key_hold.finish_suspended(service).await
                };
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                meta.note_transition(service, key_id, t);
            }
            ReportMode::Retryable => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    // Proxied transport failure (tunnel error through a leased
                    // node) blames the node — a dead proxy accumulates
                    // consecutive_fails and self-disables (HEAD semantics).
                    if let Err(ProviderError::Http(e)) = &result {
                        if is_tunnel_error(e) {
                            h.finish_failure(Some(&crate::hold::safe_node_error(&e.to_string())))
                                .await;
                        } else {
                            h.finish_release().await;
                        }
                    } else {
                        h.finish_release().await;
                    }
                }
            }
        }

        meta.note_attempt(service, key_id, node_id, result.is_ok());
        let upstream_status = match &result {
            Err(ProviderError::Upstream { status, .. }) => Some(*status),
            _ => None,
        };
        // The LABEL is classified from the vendor error itself, never from
        // `verdict`: `verdict` is the hold disposition chosen by the caller's
        // report closure, and the plain-`Failure` legs (tavily research) would
        // otherwise collapse a real 401/402/429 into "failure" while still
        // recording its real status. Disposition and label are unrelated
        // concerns; the hold behavior above is unchanged.
        let label = match &result {
            Ok(_) => "ok",
            Err(e) => outcome_label(verdict_for(service, e), upstream_status),
        };
        meta.note_outcome(service, key_id, label, upstream_status);
        ctx.observe_meta(meta);
        (result, verdict)
    }
    .instrument(span.clone())
    .await;

    // Outcome label recorded on the span handle (no enter needed).
    span.record(
        "outcome",
        match verdict {
            ReportMode::Ok => "ok",
            ReportMode::Exhausted => "exhausted",
            ReportMode::PaymentRequired => "payment_required",
            ReportMode::AuthFailure => "auth",
            ReportMode::Banned => "banned",
            ReportMode::Retryable => "retryable",
            ReportMode::Failure => "error",
        },
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serpotter_db::Db;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        is_tunnel_error, try_build_http, ExaClient, FirecrawlClient, ProviderError,
        ProviderRegistry, TavilyClient, XaiClient,
    };

    use crate::hold::{KeyRefresh, ProxyRefresh};
    use crate::meta::{ExecMeta, ProgressEvent, ProgressSink};
    use crate::ProductCtx;

    /// Whole seconds between two db-rendered `datetime('now')`-format stamps.
    /// Parsed here (no chrono dep) so a cooldown test never trusts the test
    /// host's wall clock against the db's.
    fn age_secs(then: &str, now: &str) -> i64 {
        let parse = |s: &str| -> i64 {
            let t: Vec<i64> = s
                .split(['-', ' ', ':'])
                .map(|p| p.trim().parse().unwrap_or(0))
                .collect();
            // y, m, d, h, m, s
            ((t[0] * 372 + t[1] * 31 + t[2]) * 24 + t[3]) * 60 * 60 + t[4] * 60 + t[5]
        };
        parse(then) - parse(now)
    }

    use super::{
        cooldown_secs_for, outcome_label, verdict_for, with_key_proxy, LeaseError, ReportMode,
        DEFAULT_COOLDOWN_SECS, MAX_COOLDOWN_SECS,
    };

    /// Live Firecrawl ban body, copied verbatim from
    /// `search::banned::FIRECRAWL_BAN_BODY_FIXTURE` (module-private there).
    const BAN_BODY: &str = r#"{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}"#;

    fn upstream(provider: &str, status: u16) -> ProviderError {
        ProviderError::Upstream {
            provider: provider.to_string(),
            status,
            body: String::new(),
            retry_after_secs: None,
        }
    }

    /// Migrated in-memory db with one key for `service` and one http node.
    async fn seed_db(service: &str) -> (Db, i64, i64) {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let key = db
            .insert_api_key(service, "sk-test-secret")
            .await
            .expect("key");
        let node = db
            .insert_node("127.0.0.1", 9, None, None, "http")
            .await
            .expect("node");
        (db, key.id, node.id)
    }

    fn registry() -> ProviderRegistry {
        ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        )
    }

    fn ctx_for(db: Db, require_proxy: bool) -> ProductCtx {
        let keys = Arc::new(KeyPool::new(db.clone()));
        let outbound = Arc::new(ProxyPool::with_options(db.clone(), require_proxy));
        ProductCtx {
            db,
            keys,
            outbound,
            providers: registry(),
            progress: None,
            meta_sink: None,
            request_timeout: Duration::from_secs(120),
            cache_enabled: false,
            cache_ttl: Duration::from_secs(300),
        }
    }

    /// Run one `with_key_proxy` call: acquire_err identity (E = `LeaseError`),
    /// the given `mode` as the report verdict, and a call closure producing
    /// exactly `err` (or Ok when `err` is None).
    async fn run_one(
        db: Db,
        service: &str,
        require_proxy: bool,
        direct: bool,
        err: Option<ProviderError>,
        mode: ReportMode,
    ) -> (Result<Result<String, ProviderError>, LeaseError>, ExecMeta) {
        let ctx = ctx_for(db, require_proxy);
        let mut meta = ExecMeta::default();
        let outcome = with_key_proxy(
            &ctx,
            service,
            direct,
            1,
            3,
            &mut meta,
            |e| e,
            |_| mode,
            move |_key: String,
                  _proxy: Option<String>,
                  _client: reqwest::Client,
                  _hold: KeyRefresh,
                  _proxy_hold: Option<ProxyRefresh>| async move {
                match err {
                    Some(e) => Err(e),
                    None => Ok("done".to_string()),
                }
            },
        )
        .await;
        (outcome, meta)
    }

    /// `402` (out of money) and `429`/`432`/`433` (rate/plan limits) both count
    /// as "exhausted" upstream, but they MUST diverge here: only `402` may zero
    /// a key's credits. This group is the guard that a rate limit never
    /// permanently demotes a healthy account.
    #[test]
    fn verdict_for_rate_limits_stays_exhausted() {
        for (provider, status) in [
            ("tavily", 429),
            ("tavily", 432),
            ("tavily", 433),
            ("firecrawl", 429),
            ("exa", 429),
            ("xai", 429),
        ] {
            assert_eq!(
                verdict_for(provider, &upstream(provider, status)),
                ReportMode::Exhausted,
                "{provider} {status} must stay on the credit-preserving path"
            );
        }
    }

    #[test]
    fn verdict_for_payment_required() {
        // Ordered before the exhausted guard on purpose: 402 is ALSO an
        // exhausted status for firecrawl/exa/unknown, so a reorder would
        // silently restore the never-demoted `402` loop.
        for provider in ["firecrawl", "exa", "unknown"] {
            assert_eq!(
                verdict_for(provider, &upstream(provider, 402)),
                ReportMode::PaymentRequired,
                "{provider} 402 must demote the key"
            );
        }
    }

    #[test]
    fn verdict_for_firecrawl_banned() {
        let banned = ProviderError::Upstream {
            provider: "firecrawl".into(),
            status: 403,
            body: BAN_BODY.into(),
            retry_after_secs: None,
        };
        assert_eq!(
            verdict_for("firecrawl", &banned),
            ReportMode::Banned,
            "ban-body 403 must beat AuthFailure"
        );
        let banned401 = ProviderError::Upstream {
            provider: "firecrawl".into(),
            status: 401,
            body: "account has been banned".into(),
            retry_after_secs: None,
        };
        assert_eq!(verdict_for("firecrawl", &banned401), ReportMode::Banned);
        // Same body on a non-firecrawl provider is a likely-tier ban, which
        // DISABLES the key with `disabled_reason = 'vendor_suspended'` —
        // permanent since schema 18 (the re-enable cron skips that reason), so
        // an operator re-enable is the only way back.
        assert_eq!(verdict_for("tavily", &banned), ReportMode::Banned);
        // Plain 403 Unauthorized body (no ban markers) is auth, not banned.
        assert_eq!(
            verdict_for(
                "firecrawl",
                &ProviderError::Upstream {
                    provider: "firecrawl".into(),
                    status: 403,
                    body: r#"{"success":false,"error":"Unauthorized"}"#.into(),
                    retry_after_secs: None,
                }
            ),
            ReportMode::AuthFailure
        );
    }

    #[test]
    fn verdict_for_auth_retryable_failure() {
        // 401/403 (not exhausted, not banned) → AuthFailure.
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 401)),
            ReportMode::AuthFailure
        );
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 403)),
            ReportMode::AuthFailure
        );
        // 429/5xx that is NOT an exhausted status for that provider → Retryable.
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 503)),
            ReportMode::Retryable
        );
        assert_eq!(
            verdict_for("xai", &upstream("xai", 500)),
            ReportMode::Retryable
        );
        assert_eq!(
            verdict_for("unknown", &upstream("unknown", 429)),
            ReportMode::Retryable
        );
        // Everything else → Failure.
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 400)),
            ReportMode::Failure
        );
        assert_eq!(
            verdict_for(
                "tavily",
                &ProviderError::Unextractable {
                    provider: "tavily".into(),
                    message: "empty page".into()
                }
            ),
            ReportMode::Failure
        );
        assert_eq!(
            verdict_for(
                "tavily",
                &ProviderError::Unsupported {
                    provider: "tavily".into(),
                    action: "search",
                    detail: "over cap".into()
                }
            ),
            ReportMode::Failure
        );
    }

    #[test]
    fn verdict_for_http_is_retryable() {
        let http_err = match try_build_http(Some("not-a-url-:::")).unwrap_err() {
            ProviderError::Http(e) => e,
            other => panic!("expected Http error, got {other:?}"),
        };
        assert_eq!(
            verdict_for("tavily", &ProviderError::Http(http_err)),
            ReportMode::Retryable,
            "transport errors retry the same account (HEAD parity)"
        );
    }

    #[tokio::test]
    async fn ok_mode_finishes_key_and_node_success() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        let (outcome, meta) =
            run_one(db.clone(), "tavily", false, false, None, ReportMode::Ok).await;
        assert!(matches!(&outcome, Ok(Ok(s)) if s == "done"));
        assert_eq!(meta.attempt_count, 1);
        assert_eq!(meta.key_id, Some(key_id));
        assert_eq!(meta.node_id, Some(node_id));
        let key = db.get_api_key(key_id).await.unwrap().unwrap();
        assert_eq!(key.active, 1);
        assert_eq!(key.consecutive_fails, 0, "success resets fails");
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(node.inflight, 0, "node hold released by finish_success");
        assert_eq!(node.consecutive_fails, 0);
    }

    /// C3a: long-poll refresh inside the call closure. The closure expires
    /// both leases, refreshes key+node holds mid-call (as the structured
    /// extract poll does every 2s tick), observes the leases moved forward,
    /// and the ladder STILL finishes both holds per the Ok verdict after the
    /// call returns — refresh never interferes with finish semantics.
    #[tokio::test]
    async fn refresh_inside_call_then_finish_per_verdict() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        let keys = Arc::new(KeyPool::new(db.clone()));
        let outbound = Arc::new(ProxyPool::with_options(db.clone(), false));
        let ctx = ProductCtx {
            db: db.clone(),
            keys,
            outbound,
            providers: registry(),
            progress: None,
            meta_sink: None,
            request_timeout: Duration::from_secs(120),
            cache_enabled: false,
            cache_ttl: Duration::from_secs(300),
        };
        let mut meta = ExecMeta::default();
        let outcome = with_key_proxy(
            &ctx,
            "tavily",
            false,
            1,
            1,
            &mut meta,
            |e| e,
            |_| ReportMode::Ok,
            |_key: String,
             _proxy: Option<String>,
             _client: reqwest::Client,
             hold: KeyRefresh,
             proxy_hold: Option<ProxyRefresh>| {
                // Clone for the async block: edition-2024 capture inference
                // moves `db` into a `move` async block inside a generic FnOnce.
                let dbc = db.clone();
                async move {
                    // Simulate a long-poll tick: force the key lease to an
                    // ancient value, then refresh both holds before "sleeping".
                    dbc.set_api_key_lease_until(key_id, Some("2000-01-01 00:00:00"))
                        .await
                        .unwrap();
                    hold.refresh().await;
                    let ph = proxy_hold.expect("proxied leg must carry a proxy hold");
                    ph.refresh().await;
                    // The key lease must have moved forward off the ancient value;
                    // the node lease stays alive under the refresh.
                    let key = dbc.get_api_key_admin(key_id).await.unwrap().unwrap();
                    assert!(
                        key.lease_until.as_deref() != Some("2000-01-01 00:00:00"),
                        "key lease refreshed mid-call (ancient value must move forward): {:?}",
                        key.lease_until
                    );
                    assert!(key.lease_until.is_some(), "key lease still live");
                    let node = dbc.get_node(node_id).await.unwrap().unwrap();
                    assert!(node.lease_until.is_some(), "node lease kept alive");
                    Ok("done".to_string())
                }
            },
        )
        .await;
        assert!(
            matches!(&outcome, Ok(Ok(s)) if s == "done"),
            "refresh must not change the outcome: {outcome:?}"
        );
        assert_eq!(meta.attempt_count, 1);
        // The ladder still finished both holds per the Ok verdict AFTER the call.
        let admin = db.get_api_key_admin(key_id).await.unwrap().unwrap();
        assert_eq!(admin.inflight, 0, "success finished the key hold");
        assert_eq!(admin.lease_until, None, "last hold cleared the lease");
        let key = db.get_api_key(key_id).await.unwrap().unwrap();
        assert_eq!(key.consecutive_fails, 0, "success resets fails");
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(node.inflight, 0, "success finished the node hold");
        assert_eq!(node.consecutive_fails, 0);
    }

    #[tokio::test]
    async fn failure_mode_releases_key_and_node() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        let err = ProviderError::Unsupported {
            provider: "tavily".into(),
            action: "search",
            detail: "boom".into(),
        };
        let (outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Failure,
        )
        .await;
        assert!(
            matches!(&outcome, Ok(Err(ProviderError::Unsupported { .. }))),
            "failure must surface the provider error: {outcome:?}"
        );
        assert_eq!(meta.attempt_count, 1);
        assert_eq!(meta.key_id, Some(key_id));
        let key = db.get_api_key(key_id).await.unwrap().unwrap();
        assert_eq!(key.active, 1, "Failure must not hard-disable the key");
        assert_eq!(key.consecutive_fails, 0, "Failure releases without fail@3");
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(node.inflight, 0);
        assert_eq!(node.consecutive_fails, 0);
    }

    #[tokio::test]
    async fn exhausted_mode_zeroes_key_credits_and_releases_node() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        db.set_api_key_credits(key_id, Some(10)).await.unwrap();
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 429,
            body: "plan limit".into(),
            retry_after_secs: None,
        };
        let (outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Exhausted,
        )
        .await;
        assert!(matches!(
            &outcome,
            Ok(Err(ProviderError::Upstream { status: 429, .. }))
        ));
        let key = db.get_api_key_admin(key_id).await.unwrap().unwrap();
        assert_eq!(
            key.credits_remaining,
            Some(0),
            "exhausted zeroes tracked credits"
        );
        assert_eq!(key.inflight, 0, "exhausted decrements inflight");
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(
            node.inflight, 0,
            "node released, never blamed on exhaustion"
        );
        assert_eq!(node.consecutive_fails, 0);
    }

    /// The vendor's `Retry-After` becomes the stamped cooldown, capped at
    /// [`MAX_COOLDOWN_SECS`]; absent it, [`DEFAULT_COOLDOWN_SECS`].
    #[tokio::test]
    async fn exhausted_cooldown_uses_retry_after_capped_at_one_hour() {
        let (db, key_id, _node) = seed_db("tavily").await;
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 429,
            body: "plan limit".into(),
            retry_after_secs: Some(9999),
        };
        let (_outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Exhausted,
        )
        .await;
        let cooldown = db
            .get_api_key_cooldown(key_id)
            .await
            .unwrap()
            .expect("a 429 must stamp a cooldown");
        let now = db.now().await.unwrap();
        assert!(cooldown > now, "a 429 must stamp a future cooldown");
        // 9999s clamped to 3600s: still future, but well under the raw value.
        let delta = age_secs(&cooldown, &now);
        assert!(
            (delta - MAX_COOLDOWN_SECS).abs() <= 5,
            "cooldown must clamp to MAX_COOLDOWN_SECS, got {delta}s"
        );
    }

    #[tokio::test]
    async fn exhausted_cooldown_defaults_to_sixty_without_retry_after() {
        let (db, key_id, _node) = seed_db("tavily").await;
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 429,
            body: "plan limit".into(),
            retry_after_secs: None,
        };
        let (_outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Exhausted,
        )
        .await;
        let cooldown = db
            .get_api_key_cooldown(key_id)
            .await
            .unwrap()
            .expect("a 429 without Retry-After still stamps the default");
        let delta = age_secs(&cooldown, &db.now().await.unwrap());
        assert!(
            (delta - DEFAULT_COOLDOWN_SECS).abs() <= 5,
            "no Retry-After must fall back to DEFAULT_COOLDOWN_SECS, got {delta}s"
        );
    }

    /// Only the exhausted verdict may ever write `cooldown_until`; an auth
    /// failure on the same key must leave a pre-existing stamp untouched.
    #[tokio::test]
    async fn non_exhausted_verdicts_never_touch_cooldown_until() {
        let (db, key_id, _node) = seed_db("tavily").await;
        for (err, mode) in [
            (
                ProviderError::Upstream {
                    provider: "tavily".into(),
                    status: 401,
                    body: "unauthorized".into(),
                    retry_after_secs: Some(600),
                },
                ReportMode::AuthFailure,
            ),
            (
                ProviderError::Upstream {
                    provider: "tavily".into(),
                    status: 503,
                    body: "busy".into(),
                    retry_after_secs: Some(600),
                },
                ReportMode::Retryable,
            ),
        ] {
            let before = db.get_api_key_cooldown(key_id).await.unwrap();
            assert_eq!(before, None, "no verdict but Exhausted may stamp it");
            let (_outcome, _meta) =
                run_one(db.clone(), "tavily", false, false, Some(err), mode).await;
            let after = db.get_api_key_cooldown(key_id).await.unwrap();
            assert_eq!(
                after, before,
                "{mode:?} must leave cooldown_until untouched even with a Retry-After"
            );
        }
    }

    /// `cooldown_secs_for` is the single mapping from a vendor error to a
    /// stamped cooldown; pin both ends of the clamp/default rule.
    #[test]
    fn cooldown_secs_for_maps_retry_after_and_default() {
        let for_secs = |secs: Option<u64>| -> Result<(), ProviderError> {
            Err(ProviderError::Upstream {
                provider: "tavily".into(),
                status: 429,
                body: String::new(),
                retry_after_secs: secs,
            })
        };
        assert_eq!(cooldown_secs_for(&for_secs(Some(9999))), MAX_COOLDOWN_SECS);
        assert_eq!(cooldown_secs_for(&for_secs(Some(120))), 120);
        assert_eq!(cooldown_secs_for(&for_secs(None)), DEFAULT_COOLDOWN_SECS);
        // A `Retry-After` at or above 2^63 is the wrap hazard: casting to i64
        // first would yield a NEGATIVE value that `.min(3600)` passes through
        // untouched, and the db's `.max(0)` would stamp `'+0 seconds'` — a
        // dropped cooldown. Both must clamp to the ceiling instead.
        assert_eq!(
            cooldown_secs_for(&for_secs(Some(1u64 << 63))),
            MAX_COOLDOWN_SECS,
            "2^63 must not wrap negative and bypass the clamp"
        );
        assert_eq!(
            cooldown_secs_for(&for_secs(Some(u64::MAX))),
            MAX_COOLDOWN_SECS,
            "u64::MAX must not wrap negative and bypass the clamp"
        );
        assert!(cooldown_secs_for(&for_secs(Some(u64::MAX))) > 0);
    }

    #[tokio::test]
    async fn auth_failure_increments_key_fails() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 401,
            body: "unauthorized".into(),
            retry_after_secs: None,
        };
        let (outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::AuthFailure,
        )
        .await;
        assert!(matches!(
            &outcome,
            Ok(Err(ProviderError::Upstream { status: 401, .. }))
        ));
        let key = db.get_api_key(key_id).await.unwrap().unwrap();
        assert_eq!(
            key.consecutive_fails, 1,
            "AuthFailure is the only fail@3 signal"
        );
        assert_eq!(key.active, 1, "not yet at the 3-fail threshold");
        let admin = db.get_api_key_admin(key_id).await.unwrap().unwrap();
        assert_eq!(admin.inflight, 0);
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(
            node.inflight, 0,
            "proxy released on auth class, never blamed"
        );
    }

    #[tokio::test]
    async fn banned_mode_deletes_key() {
        let (db, key_id, _node_id) = seed_db("firecrawl").await;
        let err = ProviderError::Upstream {
            provider: "firecrawl".into(),
            status: 403,
            body: BAN_BODY.into(),
            retry_after_secs: None,
        };
        let (outcome, _meta) = run_one(
            db.clone(),
            "firecrawl",
            false,
            false,
            Some(err),
            ReportMode::Banned,
        )
        .await;
        assert!(matches!(
            &outcome,
            Ok(Err(ProviderError::Upstream { status: 403, .. }))
        ));
        assert!(
            db.get_api_key(key_id).await.unwrap().is_none(),
            "Banned hard-deletes the key row"
        );
    }

    #[tokio::test]
    async fn banned_mode_suspends_non_firecrawl_key() {
        let (db, key_id, _node_id) = seed_db("tavily").await;
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 403,
            body: r#"{"error":"account suspended"}"#.into(),
            retry_after_secs: None,
        };
        let (outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Banned,
        )
        .await;
        assert!(matches!(
            &outcome,
            Ok(Err(ProviderError::Upstream { status: 403, .. }))
        ));
        let row = db
            .get_api_key(key_id)
            .await
            .unwrap()
            .expect("suspended key row survives");
        assert_eq!(row.active, 0, "likely-tier ban disables the key");
        assert_eq!(
            row.consecutive_fails, 0,
            "suspension never counts auth strikes"
        );
    }

    #[tokio::test]
    async fn retryable_mode_releases_both_without_fails() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 503,
            body: "busy".into(),
            retry_after_secs: None,
        };
        let (outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Retryable,
        )
        .await;
        assert!(matches!(
            &outcome,
            Ok(Err(ProviderError::Upstream { status: 503, .. }))
        ));
        let key = db.get_api_key(key_id).await.unwrap().unwrap();
        assert_eq!(key.consecutive_fails, 0, "Retryable never fail@3s the key");
        assert_eq!(key.active, 1);
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(node.inflight, 0);
        assert_eq!(node.consecutive_fails, 0);
    }

    /// The tunnel branch's two real properties: a proxied transport failure
    /// BLAMES the node (`consecutive_fails`++, disable at 3) while the key
    /// stays release-only, and it records a non-empty `last_error` an operator
    /// can act on.
    ///
    /// It deliberately does NOT assert credential redaction here. Measured
    /// against reqwest 0.12, no error it produces on this path echoes the
    /// proxy URL (`hold::redact_url_userinfo`'s doc records the measurement),
    /// so such an assertion would pass whether or not the ladder redacted —
    /// a guard that cannot fail is worse than none. The redaction itself is
    /// pinned as a unit on the helper, where the input is under our control.
    #[tokio::test]
    async fn tunnel_failure_blames_the_node_and_records_last_error() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        // A genuine connect-class error, classified by `is_tunnel_error` and
        // handed to the ladder as the leased node's failure. NOTE: this client
        // has no proxy configured, so the connect goes direct to 127.0.0.1:9 —
        // the seeded node exists here as the DB row the ladder blames, not as
        // a traversed proxy.
        let tunnel = reqwest::Client::new()
            .get("http://127.0.0.1:9/")
            .send()
            .await
            .expect_err("127.0.0.1:9 refuses connections");
        assert!(
            is_tunnel_error(&tunnel),
            "fixture must be a tunnel-class error: {tunnel}"
        );
        let (outcome, _meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(ProviderError::Http(tunnel)),
            ReportMode::Retryable,
        )
        .await;
        assert!(matches!(&outcome, Ok(Err(ProviderError::Http(_)))));
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(node.consecutive_fails, 1, "tunnel blames the node");
        assert_eq!(node.inflight, 0, "the hold is finished either way");
        assert!(
            node.last_error.is_some_and(|e| !e.is_empty()),
            "an operator must be able to see why the node was blamed"
        );
        let key = db.get_api_key(key_id).await.unwrap().unwrap();
        assert_eq!(
            key.consecutive_fails, 0,
            "a tunnel failure never fail@3s the key"
        );
    }

    #[tokio::test]
    async fn direct_skips_outbound_even_when_required() {
        // require_proxy=true, NO node, direct=true → xAI must still succeed.
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        db.insert_api_key("xai", "sk-test-secret").await.unwrap();
        let (outcome, meta) = run_one(db.clone(), "xai", true, true, None, ReportMode::Ok).await;
        assert!(
            matches!(&outcome, Ok(Ok(s)) if s == "done"),
            "direct must skip outbound even under REQUIRE_OUTBOUND_PROXY: {outcome:?}"
        );
        assert_eq!(meta.node_id, None, "direct never touches outbound");
        assert_eq!(db.count_nodes().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn direct_leaves_present_node_untouched() {
        let (db, _key_id, node_id) = seed_db("xai").await;
        let (outcome, meta) = run_one(db.clone(), "xai", false, true, None, ReportMode::Ok).await;
        assert!(matches!(&outcome, Ok(Ok(_))));
        assert_eq!(meta.node_id, None);
        let node = db.get_node(node_id).await.unwrap().unwrap();
        assert_eq!(node.inflight, 0, "direct must not acquire a node");
        assert_eq!(node.consecutive_fails, 0);
    }

    #[tokio::test]
    async fn acquire_no_healthy_key_maps_error() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        // No key inserted for the service.
        let (outcome, meta) = run_one(db, "tavily", false, false, None, ReportMode::Ok).await;
        assert!(
            matches!(&outcome, Err(LeaseError::NoHealthyKey(s)) if s == "No healthy tavily key"),
            "empty inventory must map to NoHealthyKey: {outcome:?}"
        );
        assert_eq!(meta.attempt_count, 0, "no attempt without a key");
    }

    #[tokio::test]
    async fn acquire_busy_maps_key_busy() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let _key = db.insert_api_key("tavily", "sk-test-secret").await.unwrap();
        let keys = Arc::new(KeyPool::with_config(
            db.clone(),
            1,
            Duration::from_millis(150),
            90,
            100,
        ));
        let outbound = Arc::new(ProxyPool::with_options(db.clone(), false));
        let ctx = ProductCtx {
            db: db.clone(),
            keys: Arc::clone(&keys),
            outbound,
            providers: registry(),
            progress: None,
            meta_sink: None,
            request_timeout: Duration::from_secs(120),
            cache_enabled: false,
            cache_ttl: Duration::from_secs(300),
        };
        // Occupy the only slot: a real acquire bumps inflight to max_inflight
        // and stamps a 90s lease_until, so the second acquire waits out its
        // 150ms deadline and must map to AcquireTimeout (KeyHold::new alone
        // never touches the row, so a bare guard would not block anything).
        let _occupied = keys.acquire("tavily").await.expect("first acquire");
        let mut meta = ExecMeta::default();
        let outcome = with_key_proxy(
            &ctx,
            "tavily",
            false,
            1,
            3,
            &mut meta,
            |e| e,
            |_| ReportMode::Failure,
            |_key: String,
             _proxy: Option<String>,
             _client: reqwest::Client,
             _hold: KeyRefresh,
             _proxy_hold: Option<ProxyRefresh>| async move { Ok("done".to_string()) },
        )
        .await;
        match &outcome {
            Err(LeaseError::KeyBusy(s)) => {
                assert_eq!(s, "All tavily keys busy (acquire timeout)");
            }
            other => panic!("at-cap inventory through deadline must map to KeyBusy: {other:?}"),
        }
        // The lease and its inflight die with the in-memory DB; nothing to clean.
    }

    #[tokio::test]
    async fn acquire_no_healthy_node_when_require_proxy() {
        // Key present, NO node: require_proxy=true + non-direct must map to
        // NoHealthyNode (and release the acquired key before returning).
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let key = db.insert_api_key("tavily", "sk-test-secret").await.unwrap();
        let (outcome, _meta) =
            run_one(db.clone(), "tavily", true, false, None, ReportMode::Ok).await;
        match &outcome {
            Err(LeaseError::NoHealthyNode(s)) => {
                assert_eq!(s, "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)");
            }
            other => panic!("require_proxy with no node must map to NoHealthyNode: {other:?}"),
        }
        let key = db.get_api_key_admin(key.id).await.unwrap().unwrap();
        assert_eq!(key.inflight, 0, "key released before NoHealthyNode return");
    }

    /// A node whose host the URL parser rejects (space) → `client_for` Errs
    /// before any dial. The KEY is released (our config is what broke, the
    /// account is fine), but the NODE is BLAMED: a proxy URL that does not
    /// build is a node-configuration defect, and a released node sits at
    /// inflight 0 — exactly what `inflight ASC, id ASC` picks first, forever.
    /// Without the fail++ this node kills every proxied leg.
    #[tokio::test]
    async fn client_for_error_blames_node_and_releases_key() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        db.insert_api_key("tavily", "sk-test-secret").await.unwrap();
        db.insert_node("ho st", 9, None, None, "http")
            .await
            .unwrap();
        let (outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            None,
            ReportMode::Failure,
        )
        .await;
        assert!(
            matches!(&outcome, Ok(Err(ProviderError::Http(_)))),
            "bad proxied URL must fail at client_for, not dial: {outcome:?}"
        );
        let keys = db.list_api_keys().await.unwrap();
        assert_eq!(keys.len(), 1, "client_for failure never fails the key");
        assert_eq!(keys[0].consecutive_fails, 0);
        assert_eq!(keys[0].inflight, 0);
        assert_eq!(meta.attempt_count, 1);
        assert_eq!(meta.key_id, Some(keys[0].id));
        let nodes = db.list_nodes().await.unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].inflight, 0, "the hold is finished either way");
        assert_eq!(
            nodes[0].consecutive_fails, 1,
            "an unbuildable proxy URL is a node defect: the node must accumulate \
             fails so fail@3 (disable) is reachable — a released node stays at \
             inflight 0 and wins every `inflight ASC, id ASC` pick forever"
        );
        assert!(
            !nodes[0].last_error.as_deref().unwrap_or("").is_empty(),
            "the build error is recorded on the node row for the operator"
        );
    }

    /// Recording sink for emission-ownership assertions.
    #[derive(Clone, Default)]
    struct VecSink(Arc<std::sync::Mutex<Vec<ProgressEvent>>>);

    impl ProgressSink for VecSink {
        fn emit(&self, event: &ProgressEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    #[tokio::test]
    async fn attempt_event_emitted_with_attempt_and_max() {
        let (db, _key_id, _node_id) = seed_db("tavily").await;
        let keys = Arc::new(KeyPool::new(db.clone()));
        let outbound = Arc::new(ProxyPool::with_options(db.clone(), false));
        let sink = VecSink::default();
        let ctx = ProductCtx {
            db,
            keys,
            outbound,
            providers: registry(),
            progress: Some(Arc::new(sink.clone())),
            meta_sink: None,
            request_timeout: Duration::from_secs(120),
            cache_enabled: false,
            cache_ttl: Duration::from_secs(300),
        };
        let mut meta = ExecMeta::default();
        let _ = with_key_proxy(
            &ctx,
            "tavily",
            false,
            2,
            3,
            &mut meta,
            |e| e,
            |_| ReportMode::Failure,
            |_key: String,
             _proxy: Option<String>,
             _client: reqwest::Client,
             _hold: KeyRefresh,
             _proxy_hold: Option<ProxyRefresh>| async move { Ok("done".to_string()) },
        )
        .await;
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![ProgressEvent::Attempt {
                service: "tavily".into(),
                attempt: 2,
                max: 3,
            }],
            "with_key_proxy emits exactly one Attempt (Retry is caller-owned)"
        );
    }

    /// The label set is CLOSED and total, and the mapping is pinned
    /// table-driven: a function that returned a constant would fail here, and
    /// so would a variant silently folded into a neighbour's class.
    #[test]
    fn outcome_label_maps_each_verdict_to_its_own_class() {
        let expected = [
            (ReportMode::Ok, "ok"),
            (ReportMode::Failure, "failure"),
            (ReportMode::Exhausted, "rate_limited"),
            (ReportMode::PaymentRequired, "payment_required"),
            (ReportMode::AuthFailure, "auth_invalid"),
            (ReportMode::Banned, "banned"),
            (ReportMode::Retryable, "retryable"),
        ];
        for (mode, want) in expected {
            // 403 is excluded here: it is the one status that splits
            // `AuthFailure` into `forbidden`, pinned by the split test below.
            for status in [None, Some(401), Some(402), Some(429), Some(503)] {
                let got = outcome_label(mode, status);
                assert_eq!(
                    got, want,
                    "{mode:?} must label as {want} regardless of status {status:?}"
                );
            }
        }
    }

    /// No verdict may escape the closed label set: every pair lands on one of
    /// the 8 names the metric is allowed to emit.
    #[test]
    fn outcome_label_never_leaves_the_closed_set() {
        const CLOSED: [&str; 8] = [
            "ok",
            "payment_required",
            "rate_limited",
            "auth_invalid",
            "forbidden",
            "banned",
            "retryable",
            "failure",
        ];
        for mode in [
            ReportMode::Ok,
            ReportMode::Failure,
            ReportMode::Exhausted,
            ReportMode::PaymentRequired,
            ReportMode::AuthFailure,
            ReportMode::Banned,
            ReportMode::Retryable,
        ] {
            for status in [None, Some(401), Some(402), Some(403), Some(429), Some(503)] {
                let label = outcome_label(mode, status);
                assert!(
                    CLOSED.contains(&label),
                    "{mode:?}/{status:?} produced out-of-set label {label}"
                );
            }
        }
    }

    /// `AuthFailure` is the one verdict that splits: 403 is a permission
    /// problem (forbidden), 401 an authentication one (auth_invalid).
    #[test]
    fn outcome_label_splits_auth_failure_on_403() {
        assert_eq!(
            outcome_label(ReportMode::AuthFailure, Some(403)),
            "forbidden"
        );
        assert_eq!(
            outcome_label(ReportMode::AuthFailure, Some(401)),
            "auth_invalid"
        );
        // Unreachable from the dispatch — `verdict_for` only returns
        // `AuthFailure` for a 401/403 upstream status, so a statusless call can
        // never be classified `AuthFailure` in production. Asserted purely to
        // pin that the fn is total over its argument, not as a funnel case.
        assert_eq!(outcome_label(ReportMode::AuthFailure, None), "auth_invalid");
    }

    /// A seeded 401 attempt is the auth-failure funnel step: the label and the
    /// raw upstream status BOTH have to reach the log, or the ops dashboard
    /// can report a class without ever showing which vendor status produced it.
    #[tokio::test]
    async fn dispatch_records_auth_invalid_with_upstream_status() {
        let (db, key_id, _node_id) = seed_db("tavily").await;
        let (_outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(upstream("tavily", 401)),
            ReportMode::AuthFailure,
        )
        .await;
        assert_eq!(
            meta.attempt_log,
            vec![crate::meta::AttemptRecord {
                service: "tavily".into(),
                key_id,
                outcome: "auth_invalid",
                upstream_status: Some(401),
            }]
        );
    }

    /// Out-of-money (402) is a distinct class from a rate limit: it is
    /// terminal for the key, not a condition to back off from.
    #[tokio::test]
    async fn dispatch_records_payment_required() {
        let (db, key_id, _node_id) = seed_db("tavily").await;
        let (_outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(upstream("tavily", 402)),
            ReportMode::PaymentRequired,
        )
        .await;
        assert_eq!(meta.attempt_log.len(), 1);
        assert_eq!(meta.attempt_log[0].outcome, "payment_required");
        assert_eq!(meta.attempt_log[0].upstream_status, Some(402));
        assert_eq!(meta.attempt_log[0].key_id, key_id);
    }

    /// A success has NO upstream status — carrying one would make the vendor
    /// status axis look populated on the happy path.
    #[tokio::test]
    async fn dispatch_records_ok_without_upstream_status() {
        let (db, key_id, _node_id) = seed_db("tavily").await;
        let (outcome, meta) =
            run_one(db.clone(), "tavily", false, false, None, ReportMode::Ok).await;
        assert!(matches!(outcome, Ok(Ok(_))));
        assert_eq!(
            meta.attempt_log,
            vec![crate::meta::AttemptRecord {
                service: "tavily".into(),
                key_id,
                outcome: "ok",
                upstream_status: None,
            }]
        );
    }

    /// F1: the LABEL must be classified from the vendor error, not from the
    /// hold disposition. A leg may still ask for `Failure` (it never does
    /// today — the tavily-research leg used to, and the single-attempt
    /// extract legs still pass a `Banned → AuthFailure` remap), so a real 401
    /// on its job start would be recorded as "failure" WHILE carrying status
    /// 401 — self-contradictory, and it hid the auth failures the funnel
    /// exists to count. The hold behavior is untouched: only the recorded
    /// class changes. This test pins the INDEPENDENCE, using the plain-Failure
    /// disposition so the label cannot be read off the hold behavior.
    #[tokio::test]
    async fn dispatch_label_comes_from_the_error_not_the_failure_disposition() {
        let (db, _key_id, _node_id) = seed_db("tavily").await;
        let (_outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(upstream("tavily", 401)),
            // deliberately the plain-Failure disposition, not any live leg's
            ReportMode::Failure,
        )
        .await;
        assert_eq!(
            meta.attempt_log[0].outcome, "auth_invalid",
            "a real 401 must not be laundered into failure by the hold disposition"
        );
        assert_eq!(meta.attempt_log[0].upstream_status, Some(401));
    }

    /// The same leg, a rate limit: `verdict_for` maps tavily 429 to
    /// `Exhausted`, which the label set spells `rate_limited` — again not the
    /// `Failure` the hold closure asked for.
    #[tokio::test]
    async fn dispatch_label_survives_a_failure_disposition_on_429() {
        let (db, _key_id, _node_id) = seed_db("tavily").await;
        let (_outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(upstream("tavily", 429)),
            ReportMode::Failure,
        )
        .await;
        assert_eq!(meta.attempt_log[0].outcome, "rate_limited");
        assert_eq!(meta.attempt_log[0].upstream_status, Some(429));
    }

    /// A non-upstream error (no vendor status at all) still gets a real class
    /// from `verdict_for`, never the `Failure` the hold closure asked for.
    /// `Unextractable` is used because it is the one non-`Upstream` variant
    /// constructible without a live reqwest client.
    #[tokio::test]
    async fn dispatch_labels_non_upstream_errors_from_the_error_itself() {
        let (db, _key_id, _node_id) = seed_db("tavily").await;
        let err = ProviderError::Unextractable {
            provider: "tavily".into(),
            message: "empty body".into(),
        };
        let (_outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(err),
            ReportMode::Failure,
        )
        .await;
        assert_eq!(meta.attempt_log[0].outcome, "failure");
        assert_eq!(meta.attempt_log[0].upstream_status, None);
    }

    /// A 429 must land on `rate_limited`, NOT on the 402 payment class —
    /// conflating them would page an operator to top up credits on a vendor
    /// that merely needs a backoff.
    #[tokio::test]
    async fn dispatch_records_rate_limited_not_payment_required() {
        let (db, _key_id, _node_id) = seed_db("tavily").await;
        let (_outcome, meta) = run_one(
            db.clone(),
            "tavily",
            false,
            false,
            Some(upstream("tavily", 429)),
            ReportMode::Exhausted,
        )
        .await;
        assert_eq!(meta.attempt_log[0].outcome, "rate_limited");
        assert_eq!(meta.attempt_log[0].upstream_status, Some(429));
    }
}
