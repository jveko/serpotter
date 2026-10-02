//! The once-per-day key health probe (`probe_due_keys`).
//!
//! Eligibility lives entirely in the [`Db::due_probe_keys`](serpotter_db::Db)
//! SQL predicate (active, not in a live cooldown, not stamped today) — this
//! module never re-implements any part of it. Dispositions reuse the SAME
//! [`verdict_for`] classifier the live search ladders use, so a probe sees
//! exactly the verdict a real request would. The ONLY new rule — an operator
//! decision — is the order-1 `status == 401` pre-check ahead of
//! `verdict_for`: a vendor 401 is definitive (the key is dead and gets a
//! `probe_auth_401` tombstone), while WAF/proxy middleware emits 403/407 and
//! must keep flowing through the normal classifier.
//!
//! Stamps use the database server's clock ([`Db::stamp_key_probe`]), never
//! the caller's wall clock, so probe and due-query always compare the same
//! `date('now')`.

use std::time::Duration;

use serpotter_db::{ApiKeyArchiveReason, ApiKeyRow, Db, DbError};
use serpotter_outbound::{ProxyPool, ProxyPoolError};
use serpotter_providers::{
    is_tunnel_error, ProviderError, ProviderRegistry, ProviderResult, ProviderSearchParams,
    SVC_FIRECRAWL, SVC_XAI,
};

use crate::hold::safe_node_error;
use crate::lease::{verdict_for, ReportMode};

/// The pinned probe query: a real (if minimal) search, so vendors answer with
/// the same status surface they would for production traffic.
pub const PROBE_QUERY: &str = "health check";

/// Outcome counters for one probe pass. One disposition field per row we
/// called the provider on (`probed`), plus `aborted` for a pass stopped before
/// any provider call.
#[derive(Debug, Default, PartialEq)]
pub struct ProbeStats {
    /// Rows the provider was actually called for (an aborted pass reports 0).
    pub probed: usize,
    /// `Ok` → `note_key_health_success`.
    pub ok: usize,
    /// Upstream 401 (any body) → archived with `ProbeAuth401`, row deleted.
    pub deleted_401: usize,
    /// Firecrawl ban body → archived with `VendorBanned`, row deleted.
    pub banned_deleted: usize,
    /// Ban verdict on a non-firecrawl vendor → `note_key_health_suspended`.
    pub banned_suspended: usize,
    /// Bare 403 `AuthFailure` → `note_key_health_failure` (one fail).
    pub auth_fail: usize,
    /// `Exhausted` → `note_key_health_exhausted` (credits zeroed, NO cooldown).
    pub rate_limited: usize,
    /// Upstream 402 → `note_key_health_payment_required` (credits zeroed).
    pub drained: usize,
    /// `Retryable` | `Failure` → no key-state change at all.
    pub unchanged: usize,
    /// A lease was attempted, came back `None`, and `require_proxy` is on:
    /// the pass stopped without stamping anything.
    pub aborted: bool,
}

/// What the probe does to the key row for one provider-call outcome, in the
/// spec's disposition-table order (the 401 pre-check runs BEFORE
/// [`verdict_for`] — order is load-bearing).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Disposition {
    Success,
    Deleted401,
    BannedDeleted,
    BannedSuspended,
    AuthFail,
    RateLimited,
    Drained,
    Unchanged,
}

impl Disposition {
    fn count(self, stats: &mut ProbeStats) {
        match self {
            Disposition::Success => stats.ok += 1,
            Disposition::Deleted401 => stats.deleted_401 += 1,
            Disposition::BannedDeleted => stats.banned_deleted += 1,
            Disposition::BannedSuspended => stats.banned_suspended += 1,
            Disposition::AuthFail => stats.auth_fail += 1,
            Disposition::RateLimited => stats.rate_limited += 1,
            Disposition::Drained => stats.drained += 1,
            Disposition::Unchanged => stats.unchanged += 1,
        }
    }
}

/// Classify one provider-call outcome. Pure: the network and the db stay in
/// the caller, so the order-1 rule is visible in one place.
fn classify(service: &str, result: &Result<ProviderResult, ProviderError>) -> Disposition {
    match result {
        Ok(_) => Disposition::Success,
        // Order-1 pre-check (operator decision): a vendor 401 is definitive
        // regardless of body — checked BEFORE verdict_for, whose ban matcher
        // would otherwise suspend a deactivated-account 401 in place.
        Err(ProviderError::Upstream { status: 401, .. }) => Disposition::Deleted401,
        Err(e) => match verdict_for(service, e) {
            ReportMode::Banned if service == SVC_FIRECRAWL => Disposition::BannedDeleted,
            ReportMode::Banned => Disposition::BannedSuspended,
            ReportMode::AuthFailure => Disposition::AuthFail,
            ReportMode::Exhausted => Disposition::RateLimited,
            ReportMode::PaymentRequired => Disposition::Drained,
            // verdict_for never classifies an error Ok; kept for exhaustiveness.
            ReportMode::Ok | ReportMode::Retryable | ReportMode::Failure => Disposition::Unchanged,
        },
    }
}

/// Apply the key-state write for `disposition`. `Err` means the row was NOT
/// stamped (the caller leaves it due for a restart).
async fn apply(db: &Db, row: &ApiKeyRow, disposition: Disposition) -> Result<(), DbError> {
    match disposition {
        Disposition::Success => db.note_key_health_success(row.id).await,
        Disposition::Deleted401 => db
            .archive_and_delete_api_key(row.id, ApiKeyArchiveReason::ProbeAuth401)
            .await
            .map(|_| ()),
        Disposition::BannedDeleted => db
            .archive_and_delete_api_key(row.id, ApiKeyArchiveReason::VendorBanned)
            .await
            .map(|_| ()),
        Disposition::BannedSuspended => db.note_key_health_suspended(row.id).await,
        Disposition::AuthFail => db.note_key_health_failure(row.id).await,
        // No cooldown stamp: a probe 429 carries no observed vendor window
        // (see note_key_health_exhausted).
        Disposition::RateLimited => db.note_key_health_exhausted(row.id).await,
        Disposition::Drained => db.note_key_health_payment_required(row.id).await,
        Disposition::Unchanged => Ok(()),
    }
}

/// The upstream status behind a failed call (for the destructive-arm logs);
/// `0` when the error was not an upstream response.
fn upstream_status(result: &Result<ProviderResult, ProviderError>) -> u16 {
    match result {
        Err(ProviderError::Upstream { status, .. }) => *status,
        _ => 0,
    }
}

/// Run the once-per-day health pass over every due key, sequentially,
/// sleeping `stagger` before every row except the first (no concurrency —
/// the point is a gentle, observable sweep, not throughput).
///
/// Per row: lease a proxy (never for xAI — it mirrors the live `direct`
/// path), call `search`, finish the lease, then apply the verdict. A
/// lease that comes back `None` under [`ProxyPool::require_proxy`] aborts the
/// whole pass (`aborted`, no stamp — Review Focus). A per-row db-action
/// failure warns and leaves that row due for a restart; every successfully
/// applied outcome is stamped as probed today.
pub async fn probe_due_keys(
    db: &Db,
    providers: &ProviderRegistry,
    outbound: &ProxyPool,
    stagger: Duration,
) -> Result<ProbeStats, DbError> {
    let due = db.due_probe_keys().await?;
    let mut stats = ProbeStats::default();
    for (i, row) in due.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(stagger).await;
        }

        // 1) Lease — xAI never touches outbound (the live direct=true path).
        let lease = if row.service == SVC_XAI {
            None
        } else {
            match outbound.acquire().await {
                Ok(Some(lease)) => Some(lease),
                Ok(None) => {
                    if outbound.require_proxy() {
                        tracing::warn!(
                            key_id = row.id,
                            provider = %row.service,
                            "daily probe aborted: outbound proxy required but no node available"
                        );
                        stats.aborted = true;
                        break; // NO stamp — the row must stay due
                    }
                    None // direct egress is allowed: probe without a lease
                }
                Err(ProxyPoolError::Db(e)) => {
                    // Per-row db failure: leave this row due for a restart and
                    // keep the pass going. The provider was never called, so
                    // probed must not count it.
                    tracing::warn!(
                        key_id = row.id,
                        provider = %row.service,
                        error = %e,
                        "daily probe: proxy lease failed; leaving row due"
                    );
                    continue;
                }
            }
        };

        // 2) The probe call: a minimal, pinned-shape search.
        let params = ProviderSearchParams {
            query: PROBE_QUERY,
            max_results: 1,
            api_key: &row.key,
            include_content: false,
            include_answer: false,
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: None,
            sources: None,
            include_domains: None,
            exclude_domains: None,
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: None,
            to_date: None,
            time_range: None,
            country: None,
            exact_match: None,
        };
        let result = providers
            .search(&row.service, params, lease.as_ref().map(|l| l.url.as_str()))
            .await;
        stats.probed += 1;

        // 3) Finish the proxy hold per the outcome: success blames no one,
        // a tunnel failure blames the leased node, anything else just
        // releases (matches the live ladder's hold finishing).
        if let Some(lease) = &lease {
            match &result {
                Ok(_) => {
                    let _ = outbound.report_success(lease).await;
                }
                Err(ProviderError::Http(e)) if is_tunnel_error(e) => {
                    let _ = outbound
                        .report_failure(lease, Some(&safe_node_error(&e.to_string())))
                        .await;
                }
                _ => {
                    let _ = outbound.release(lease).await;
                }
            }
        }

        // 4) Apply the verdict, then stamp a successfully-applied outcome.
        let disposition = classify(&row.service, &result);
        if let Err(e) = apply(db, row, disposition).await {
            tracing::warn!(
                key_id = row.id,
                provider = %row.service,
                error = %e,
                "daily probe: key-state write failed; leaving row due"
            );
            continue; // NO stamp — the row stays due for a restart
        }
        let status = upstream_status(&result);
        match disposition {
            Disposition::Deleted401 | Disposition::BannedDeleted => tracing::warn!(
                key_id = row.id,
                provider = %row.service,
                status,
                "daily probe removed key"
            ),
            Disposition::BannedSuspended => tracing::warn!(
                key_id = row.id,
                provider = %row.service,
                status,
                "daily probe suspended key"
            ),
            _ => {}
        }
        if let Err(e) = db.stamp_key_probe(row.id).await {
            tracing::warn!(
                key_id = row.id,
                provider = %row.service,
                error = %e,
                "daily probe: stamp failed"
            );
        }
        disposition.count(&mut stats);
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::{probe_due_keys, ProbeStats};
    use serpotter_db::{connect_and_migrate, Db};
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
    };
    use std::time::Duration;

    const DEAD_URL: &str = "http://127.0.0.1:9";

    async fn db() -> Db {
        connect_and_migrate("sqlite::memory:").await.expect("migrate")
    }

    /// One-shot scripted upstream: bind `127.0.0.1:0`, accept ONE connection,
    /// read the request head + `content-length` body, answer with a fixed
    /// `status` + JSON `body`, close. Modeled on the api suite's
    /// `tests/common/mod.rs` fixture (READ for shape): reading to the full
    /// head+body first is what keeps a partial-read answer from surfacing as a
    /// client-side transport error (which the classifier would call
    /// `retryable` and silently turn an auth fixture into a timeout one).
    /// Hermetic: loopback only, no real network.
    fn spawn_scripted(status: u16, body: &str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind scripted upstream");
        let addr = listener.local_addr().expect("scripted upstream addr");
        let body = body.to_string();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
            let mut buf: Vec<u8> = Vec::with_capacity(1024);
            let mut chunk = [0u8; 1024];
            let mut head_len: Option<usize> = None;
            while buf.len() < 64 * 1024 {
                let Ok(n) = stream.read(&mut chunk) else { break };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if head_len.is_none() {
                    head_len = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4);
                }
                let Some(hl) = head_len else { continue };
                let head = String::from_utf8_lossy(&buf[..hl]).to_lowercase();
                let want = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= hl + want {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
        });
        format!("http://{addr}")
    }

    fn providers_tavily(url: &str) -> ProviderRegistry {
        ProviderRegistry::with_clients(
            TavilyClient::new(url),
            FirecrawlClient::new(DEAD_URL),
            ExaClient::new(DEAD_URL),
            XaiClient::new(DEAD_URL),
        )
    }

    fn providers_firecrawl(url: &str) -> ProviderRegistry {
        ProviderRegistry::with_clients(
            TavilyClient::new(DEAD_URL),
            FirecrawlClient::new(url),
            ExaClient::new(DEAD_URL),
            XaiClient::new(DEAD_URL),
        )
    }

    /// Raw `last_probe_at` for `id` (`None` = no row, or NULL stamp).
    async fn stamped(db: &Db, id: i64) -> Option<String> {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT last_probe_at FROM api_keys WHERE id = ?")
                .bind(id)
                .fetch_optional(db.pool())
                .await
                .expect("last_probe_at read");
        row.flatten()
    }

    /// The tombstone reason written by `archive_and_delete_api_key`, or `None`
    /// when the key was never archived.
    async fn archive_reason(db: &Db, id: i64) -> Option<String> {
        sqlx::query_scalar("SELECT reason FROM api_keys_archive WHERE api_key_id = ?")
            .bind(id)
            .fetch_optional(db.pool())
            .await
            .expect("archive tombstone read")
    }

    /// The db's own `date('now')` — the same clock the stamp and the due
    /// query use (never a Rust wall clock).
    async fn today(db: &Db) -> String {
        sqlx::query_scalar("SELECT date('now')")
            .fetch_one(db.pool())
            .await
            .expect("db date('now')")
    }

    async fn seed_tavily(db: &Db, key: &str) -> i64 {
        db.insert_api_key("tavily", key).await.expect("seed").id
    }

    const OK_BODY: &str = r#"{"results":[{"title":"T","url":"https://t.example","content":"c"}]}"#;
    const DEACTIVATED: &str =
        "The account associated with this API key has been deactivated. If you wish to reactivate your subscription, please contact our team.";

    #[tokio::test]
    async fn probe_200_stamps_resets_fails_and_counts_ok() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-ok").await;
        db.note_key_health_failure(id).await.unwrap();
        db.note_key_health_failure(id).await.unwrap();
        assert_eq!(db.get_api_key(id).await.unwrap().unwrap().consecutive_fails, 2);

        let base = spawn_scripted(200, OK_BODY);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            stats,
            ProbeStats {
                probed: 1,
                ok: 1,
                ..ProbeStats::default()
            }
        );
        let row = db.get_api_key(id).await.unwrap().unwrap();
        assert_eq!(row.consecutive_fails, 0, "success resets fails");
        assert_eq!(
            stamped(&db, id).await,
            Some(today(&db).await),
            "the row must be stamped with the db's date('now')"
        );
    }

    #[tokio::test]
    async fn probe_401_deletes_with_probe_reason() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-401").await;

        let base = spawn_scripted(401, r#"{"error":"Unauthorized"}"#);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            stats,
            ProbeStats {
                probed: 1,
                deleted_401: 1,
                ..ProbeStats::default()
            }
        );
        assert!(
            db.get_api_key(id).await.unwrap().is_none(),
            "a definitive vendor 401 must delete the live row"
        );
        assert_eq!(
            archive_reason(&db, id).await.as_deref(),
            Some("probe_auth_401"),
            "the probe must archive under its own allowlisted reason"
        );
    }

    #[tokio::test]
    async fn probe_401_with_deactivation_body_still_deletes_probe_reason() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-401-deact").await;

        let base = spawn_scripted(401, DEACTIVATED);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        // Review Focus #1: the order-1 401 pre-check runs BEFORE verdict_for —
        // the deactivation body would classify as Banned (suspend) if the
        // classifier ran first, but a 401 is definitive: the row is GONE, not
        // suspended-in-place.
        assert_eq!(stats.deleted_401, 1);
        assert_eq!(stats.banned_suspended, 0, "401 must not reach the ban arm");
        assert!(
            db.get_api_key(id).await.unwrap().is_none(),
            "the row must be deleted, NOT kept-and-suspended"
        );
        assert_eq!(
            archive_reason(&db, id).await.as_deref(),
            Some("probe_auth_401")
        );
    }

    #[tokio::test]
    async fn probe_firecrawl_403_ban_body_deletes_vendor_banned() {
        let db = db().await;
        let id = db
            .insert_api_key("firecrawl", "fc-probe-banned")
            .await
            .expect("seed")
            .id;

        let base = spawn_scripted(403, r#"{"error":"ACCOUNT HAS BEEN BANNED by ops"}"#);
        let providers = providers_firecrawl(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            stats,
            ProbeStats {
                probed: 1,
                banned_deleted: 1,
                ..ProbeStats::default()
            }
        );
        assert!(
            db.get_api_key(id).await.unwrap().is_none(),
            "a firecrawl ban body must delete the row, like the live ban path"
        );
        assert_eq!(
            archive_reason(&db, id).await.as_deref(),
            Some("vendor_banned")
        );
    }

    #[tokio::test]
    async fn probe_tavily_403_deactivation_suspends() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-deactivated").await;

        let base = spawn_scripted(403, DEACTIVATED);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            stats,
            ProbeStats {
                probed: 1,
                banned_suspended: 1,
                ..ProbeStats::default()
            }
        );
        let row = db.get_api_key(id).await.unwrap().expect("row KEPT");
        assert_eq!(row.active, 0, "a vendor deactivation suspends, not deletes");
        let admin = db.get_api_key_admin(id).await.unwrap().unwrap();
        assert_eq!(admin.disabled_reason.as_deref(), Some("vendor_suspended"));
        assert_eq!(archive_reason(&db, id).await, None, "no tombstone on suspend");
    }

    #[tokio::test]
    async fn probe_bare_403_counts_fail_and_keeps_row() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-403").await;

        let base = spawn_scripted(403, r#"{"error":"Forbidden"}"#);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        // Review Focus #4: a bare 403 carries no account-state copy — it is a
        // plain AuthFailure (one fail toward fail@3), never a suspend/delete.
        assert_eq!(
            stats,
            ProbeStats {
                probed: 1,
                auth_fail: 1,
                ..ProbeStats::default()
            }
        );
        let row = db.get_api_key(id).await.unwrap().unwrap();
        assert_eq!(row.consecutive_fails, 1, "bare 403 counts one fail");
        assert_eq!(row.active, 1, "one fail must not disable the key");
        assert_eq!(
            archive_reason(&db, id).await,
            None,
            "no archive row for a bare 403"
        );
    }

    #[tokio::test]
    async fn probe_429_zeroes_credits_without_cooldown() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-429").await;
        db.set_api_key_credits(id, Some(50)).await.unwrap();

        let base = spawn_scripted(429, r#"{"error":"rate limited"}"#);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(stats.rate_limited, 1);
        assert_eq!(stats.probed, 1);
        let admin = db.get_api_key_admin(id).await.unwrap().unwrap();
        assert_eq!(
            admin.credits_remaining,
            Some(0),
            "an exhausted verdict must zero tracked credits"
        );
        assert_eq!(
            db.get_api_key_cooldown(id).await.unwrap(),
            None,
            "a probe 429 stamps NO cooldown — there is no observed Retry-After window"
        );
    }

    #[tokio::test]
    async fn probe_402_zeroes_credits() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-402").await;
        db.set_api_key_credits(id, Some(50)).await.unwrap();

        let base = spawn_scripted(402, r#"{"error":"payment required"}"#);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(stats.drained, 1);
        let admin = db.get_api_key_admin(id).await.unwrap().unwrap();
        assert_eq!(admin.credits_remaining, Some(0), "402 zeroes credits");
    }

    #[tokio::test]
    async fn probe_503_changes_nothing_but_stamps() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-503").await;
        db.note_key_health_failure(id).await.unwrap();
        db.set_api_key_credits(id, Some(50)).await.unwrap();

        let base = spawn_scripted(503, r#"{"error":"upstream hiccup"}"#);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(
            stats,
            ProbeStats {
                probed: 1,
                unchanged: 1,
                ..ProbeStats::default()
            }
        );
        let row = db.get_api_key(id).await.unwrap().unwrap();
        assert_eq!(row.consecutive_fails, 1, "5xx must not count a fail");
        assert_eq!(row.active, 1);
        let admin = db.get_api_key_admin(id).await.unwrap().unwrap();
        assert_eq!(admin.credits_remaining, Some(50), "5xx must not touch credits");
        assert_eq!(
            stamped(&db, id).await,
            Some(today(&db).await),
            "an unchanged outcome is still a successfully-applied outcome: it stamps"
        );
    }

    #[tokio::test]
    async fn second_pass_same_day_probes_nothing() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-twice").await;

        let base = spawn_scripted(200, OK_BODY);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let first = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(first.probed, 1);

        // The scripted upstream serves ONE connection; a second pass reaching
        // the network would get connection-refused — but the stamp already
        // filters the row out of due_probe_keys, so the pass must short-circuit
        // before any dial.
        let second = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(
            second,
            ProbeStats::default(),
            "the second same-day pass must probe nothing"
        );
        assert_eq!(stamped(&db, id).await, Some(today(&db).await));
    }

    #[tokio::test]
    async fn inactive_and_stamped_keys_not_touched() {
        let db = db().await;
        // A: inactive (suspended) — never due.
        let a = seed_tavily(&db, "tvly-probe-inactive").await;
        db.note_key_health_suspended(a).await.unwrap();
        // B: active but already stamped today — never due.
        let b = seed_tavily(&db, "tvly-probe-stamped").await;
        db.stamp_key_probe(b).await.unwrap();
        // C: the only due row.
        let c = seed_tavily(&db, "tvly-probe-due").await;

        let due_ids: Vec<i64> = db.due_probe_keys().await.unwrap().iter().map(|r| r.id).collect();
        assert_eq!(due_ids, vec![c], "due filter must yield only C");

        let base = spawn_scripted(200, OK_BODY);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::new(db.clone());

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(stats.probed, 1, "only the due row counts as probed");
        assert_eq!(stats.ok, 1);
        assert_eq!(stamped(&db, a).await, None, "inactive row untouched");
        assert_eq!(
            stamped(&db, b).await,
            Some(today(&db).await),
            "already-stamped row not re-probed (stamp value unchanged)"
        );
        assert_eq!(stamped(&db, c).await, Some(today(&db).await));
        assert_eq!(db.get_api_key(a).await.unwrap().unwrap().active, 0);
        assert!(
            db.due_probe_keys().await.unwrap().is_empty(),
            "the whole pass must leave nothing due"
        );
    }

    #[tokio::test]
    async fn require_proxy_without_node_aborts_pass_unstamped() {
        let db = db().await;
        let id = seed_tavily(&db, "tvly-probe-abort").await;

        // require_proxy=true, NO nodes: acquire() yields None on the first
        // non-xAI row. A scripted 200 is stood up anyway so a regression that
        // dials direct still fails the assertions below (probed would be 1).
        let base = spawn_scripted(200, OK_BODY);
        let providers = providers_tavily(&base);
        let outbound = ProxyPool::with_options(db.clone(), true);

        let stats = probe_due_keys(&db, &providers, &outbound, Duration::ZERO)
            .await
            .unwrap();

        // Review Focus #3: abort means abort — no stamp, no probe.
        assert_eq!(
            stats,
            ProbeStats {
                aborted: true,
                ..ProbeStats::default()
            },
            "probed must be 0 when the pass aborts before any provider call"
        );
        assert_eq!(stamped(&db, id).await, None, "an aborted row stays unstamped");
        assert_eq!(
            db.due_probe_keys().await.unwrap().len(),
            1,
            "the row must still be due for the next attempt (restart recovers)"
        );
    }
}
