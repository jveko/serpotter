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
//! finishing — the default shared by all search legs. Callers needing
//! different semantics pass their own `report` closure (extract legs treat
//! every provider error as [`ReportMode::Failure`]).
//!
//! # Hold finishing per verdict
//!
//! | verdict     | key              | proxy            |
//! |-------------|------------------|------------------|
//! | Ok          | `finish_success` | `finish_success` |
//! | Failure     | `finish_release` | `finish_release` |
//! | Exhausted   | `finish_exhausted`| `finish_release` |
//! | AuthFailure | `finish_failure` | `finish_release` |
//! | Banned      | `finish_banned` (firecrawl, hard-delete) / `finish_suspended` (others, active=0) | `finish_release` |
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

use std::future::Future;
use std::sync::Arc;

use serpotter_keypool::KeyPoolError;
use serpotter_providers::{is_tunnel_error, ProviderError, SVC_FIRECRAWL};

use crate::hold::{KeyHold, KeyRefresh, ProxyHold, ProxyRefresh};
use crate::meta::{ExecMeta, ProgressEvent};
use crate::search::{is_account_banned, is_exhausted_status};
use crate::ProductCtx;

/// How a provider call ended; drives hold finishing (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportMode {
    Ok,
    Failure,
    Exhausted,
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

/// Default error → mode mapping (B9 semantics, shared by all search legs):
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
            if is_exhausted_status(provider, *status) {
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
/// - `client_for` failures count as a provider-call failure with
///   [`ReportMode::Failure`] semantics (release/release) and surface as
///   `Ok(Err(e))` with the exact `ProviderError` `client_for` returned.
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
    let mut key_hold = KeyHold::new(Arc::clone(&ctx.keys), lease.id);
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

    let span = tracing::info_span!(
        "provider_attempt",
        service = service,
        key_id = key_id,
        node_id = ?node_id,
        attempt = attempt,
        outcome = tracing::field::Empty,
    );
    let _guard = span.enter();

    // Build the http client for this attempt's egress (None → direct client).
    // A bad proxied URL is a provider-call failure with report=Failure:
    // release both holds, never fail@3 a healthy key on a client-build issue.
    let client = match ctx.providers.client_for(proxy_url.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            key_hold.finish_release().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
            meta.note_attempt(service, key_id, node_id, false);
            span.record("outcome", "error");
            return Ok(Err(e));
        }
    };

    let result = call(
        lease.key,
        proxy_url,
        client,
        KeyRefresh::new(Arc::clone(&ctx.keys), lease.id),
        proxy
            .as_ref()
            .map(|p| ProxyRefresh::new(Arc::clone(&ctx.outbound), p.clone())),
    )
    .await;
    let verdict = match &result {
        Ok(_) => ReportMode::Ok,
        Err(e) => report(e),
    };
    span.record(
        "outcome",
        match verdict {
            ReportMode::Ok => "ok",
            ReportMode::Exhausted => "exhausted",
            ReportMode::AuthFailure => "auth",
            ReportMode::Banned => "banned",
            ReportMode::Retryable => "retryable",
            ReportMode::Failure => "error",
        },
    );

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
            key_hold.finish_exhausted().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
        }
        ReportMode::AuthFailure => {
            key_hold.finish_failure().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
        }
        ReportMode::Banned => {
            // Two tiers: firecrawl's proven signature hard-deletes the row;
            // other vendors' likely-tier matches only disable (active=0) —
            // same instant out-of-rotation, self-heals on a false positive.
            if service == SVC_FIRECRAWL {
                key_hold.finish_banned().await;
            } else {
                key_hold.finish_suspended().await;
            }
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
        }
        ReportMode::Retryable => {
            key_hold.finish_release().await;
            if let Some(h) = proxy_hold.as_mut() {
                // Proxied transport failure (tunnel error through a leased
                // node) blames the node — a dead proxy accumulates
                // consecutive_fails and self-disables (HEAD semantics).
                if let Err(ProviderError::Http(e)) = &result {
                    if is_tunnel_error(e) {
                        h.finish_failure(Some(&crate::hold::truncate_err(&e.to_string())))
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
        try_build_http, ExaClient, FirecrawlClient, ProviderError, ProviderRegistry, TavilyClient,
        XaiClient,
    };

    use crate::hold::{KeyRefresh, ProxyRefresh};
    use crate::meta::{ExecMeta, ProgressEvent, ProgressSink};
    use crate::ProductCtx;

    use super::{verdict_for, with_key_proxy, LeaseError, ReportMode};

    /// Live Firecrawl ban body, copied verbatim from
    /// `search::banned::FIRECRAWL_BAN_BODY_FIXTURE` (module-private there).
    const BAN_BODY: &str = r#"{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}"#;

    fn upstream(provider: &str, status: u16) -> ProviderError {
        ProviderError::Upstream {
            provider: provider.to_string(),
            status,
            body: String::new(),
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

    #[test]
    fn verdict_for_exhausted_statuses() {
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 429)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 432)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("tavily", &upstream("tavily", 433)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("firecrawl", &upstream("firecrawl", 402)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("firecrawl", &upstream("firecrawl", 429)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("exa", &upstream("exa", 402)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("exa", &upstream("exa", 429)),
            ReportMode::Exhausted
        );
        assert_eq!(
            verdict_for("xai", &upstream("xai", 429)),
            ReportMode::Exhausted
        );
        // Unknown provider defaults to 402-exhausted (mysearch parity).
        assert_eq!(
            verdict_for("unknown", &upstream("unknown", 402)),
            ReportMode::Exhausted
        );
    }

    #[test]
    fn verdict_for_firecrawl_banned() {
        let banned = ProviderError::Upstream {
            provider: "firecrawl".into(),
            status: 403,
            body: BAN_BODY.into(),
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
        };
        assert_eq!(verdict_for("firecrawl", &banned401), ReportMode::Banned);
        // Same body on a non-firecrawl provider is a likely-tier ban
        // (suspends the key, reversible) rather than plain auth failure.
        assert_eq!(verdict_for("tavily", &banned), ReportMode::Banned);
        // Plain 403 Unauthorized body (no ban markers) is auth, not banned.
        assert_eq!(
            verdict_for(
                "firecrawl",
                &ProviderError::Upstream {
                    provider: "firecrawl".into(),
                    status: 403,
                    body: r#"{"success":false,"error":"Unauthorized"}"#.into()
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

    #[tokio::test]
    async fn auth_failure_increments_key_fails() {
        let (db, key_id, node_id) = seed_db("tavily").await;
        let err = ProviderError::Upstream {
            provider: "tavily".into(),
            status: 401,
            body: "unauthorized".into(),
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

    #[tokio::test]
    async fn client_for_error_is_failure_and_releases() {
        // A node whose host the URL parser rejects (space) → client_for must
        // Err; the call closure is never invoked.
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
        assert_eq!(nodes[0].inflight, 0, "proxy released on client_for failure");
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
}
