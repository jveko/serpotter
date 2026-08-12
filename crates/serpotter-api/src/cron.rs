//! Background maintenance: re-enable stale keys, purge request_log, optional credit sync.

use std::sync::Arc;
use std::time::Duration;

use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;
use sqlx::Row;
use tokio::task::JoinHandle;

const MAINT_PERIOD: Duration = Duration::from_secs(900); // 15m

/// Spawn a 15-minute interval loop for key re-enable, request_log purge,
/// and optional Tavily/Firecrawl credit sync when `CREDIT_SYNC_CRON=1`.
/// Returns a handle so the caller can abort the task on process shutdown.
///
/// The tokio interval's boot-time immediate tick is consumed eagerly, so the
/// first maintenance pass runs after one FULL period — no credit-sync storm
/// (`CREDIT_SYNC_CRON=1`) or purge burst on every process restart.
pub fn spawn_maintenance(db: Db, providers: ProviderRegistry) -> JoinHandle<()> {
    spawn_maintenance_with_period(db, providers, MAINT_PERIOD)
}

/// Like [`spawn_maintenance`] with an explicit period (tests / tuning).
pub fn spawn_maintenance_with_period(
    db: Db,
    providers: ProviderRegistry,
    period: Duration,
) -> JoinHandle<()> {
    let providers = Arc::new(providers);
    tokio::spawn(maintenance_loop(db, providers, period))
}

async fn maintenance_loop(db: Db, providers: Arc<ProviderRegistry>, period: Duration) {
    let mut tick = tokio::time::interval(period);
    // Consume the interval's immediate first tick so the first maintenance
    // pass happens after one full period (boot-time runs are unwanted: they
    // would sync every key against vendor usage limits and purge on restart).
    tick.tick().await;
    loop {
        tick.tick().await;
        run_maintenance_once(&db, &providers).await;
    }
}

/// One maintenance pass: re-enable stale keys/nodes, purge request_log and
/// expired admin_sessions, optionally sync credits. Extracted from the loop so
/// tests can drive a single pass deterministically.
async fn run_maintenance_once(db: &Db, providers: &ProviderRegistry) {
    let hours = env_i64_or("KEY_REENABLE_AFTER_HOURS", 24);
    let node_hours = env_i64_or("NODE_REENABLE_AFTER_HOURS", 24);
    let days = env_i64_or("REQUEST_LOG_RETENTION_DAYS", 30);
    let max_rows = env_i64_or("REQUEST_LOG_MAX_ROWS", 100_000);
    match db.reenable_stale_keys(hours).await {
        Ok(n) if n > 0 => tracing::info!(n, hours, "re-enabled stale api keys"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "reenable_stale_keys failed"),
    }
    match db.reenable_stale_nodes(node_hours).await {
        Ok(n) if n > 0 => {
            tracing::info!(n, hours = node_hours, "re-enabled stale outbound nodes")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "reenable_stale_nodes failed"),
    }
    match db.purge_request_log(days, max_rows).await {
        Ok(n) if n > 0 => tracing::info!(n, days, max_rows, "purged request_log rows"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge_request_log failed"),
    }
    match db.purge_expired_admin_sessions().await {
        Ok(n) if n > 0 => tracing::info!(n, "purged expired admin_sessions"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge_expired_admin_sessions failed"),
    }

    // B16: purge expired provider_jobs rows on the same cadence.
    match db.purge_expired_jobs().await {
        Ok(n) if n > 0 => tracing::info!(n, "purged expired provider_jobs"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge_expired_jobs failed"),
    }

    // B1: purge expired query-cache rows (cache_get filters them anyway; this
    // keeps the table bounded).
    match db.purge_expired_cache().await {
        Ok(n) if n > 0 => tracing::info!(n, "purged expired query-cache rows"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge_expired_cache failed"),
    }

    // B5: keep the key-pool-depth gauge fresh between ticks.
    crate::metrics::refresh_key_pool_depth(db).await;

    // B15: fire a high-error-rate alert (log + optional webhook) if the last
    // 5-minute window overshoots the threshold.
    alert_if_high_error_rate(db).await;

    // Off by default — avoid hammering vendor usage APIs every 15m.
    let credit_sync = std::env::var("CREDIT_SYNC_CRON")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if credit_sync {
        match crate::credit_sync::sync_credits_for_services(db, providers, &["tavily", "firecrawl"])
            .await
        {
            Ok(r) if r.synced > 0 || r.errors > 0 => {
                tracing::info!(
                    synced = r.synced,
                    errors = r.errors,
                    "cron credit sync finished"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "cron credit sync failed"),
        }
    }
}

/// Alert step, extracted so tests can drive it without the rest of the
/// maintenance pass: `tracing::error!` when the window ratio is exceeded,
/// then the optional webhook POST.
async fn alert_if_high_error_rate(db: &Db) {
    let Some(stats) = check_error_rate(db).await else {
        return;
    };
    tracing::error!(
        total = stats.total,
        errors = stats.errors,
        error_rate = stats.error_rate(),
        "high request error rate over the last 5 minutes"
    );
    fire_alert(stats);
}

/// Read an integer cron env var, warning (never silently) when the value is set
/// but unparseable. Missing var → `default` without a warning. `pub(crate)` so
/// the jobs module (B16) reuses it for `JOB_TTL_SECS`.
pub(crate) fn env_i64_or(key: &str, default: i64) -> i64 {
    match std::env::var(key) {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    var = key,
                    raw_value = %raw,
                    default,
                    "cron env value is not a valid integer; using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// --- B15: high-error-rate alerting ------------------------------------------

/// Alert window: the last 5 minutes of request_log rows.
pub(crate) const ALERT_WINDOW_MINUTES: i64 = 5;
/// Only alert when at least this many requests were logged in the window
/// (a noisy 2-request sample must never page anyone).
pub(crate) const ALERT_MIN_TOTAL: i64 = 20;
/// Alert when `errors / total > 0.5` (strictly greater — exactly half is not
/// "high error rate").
pub(crate) const ALERT_ERROR_RATIO: f64 = 0.5;

/// Computed 5-minute error-rate snapshot, ready to alert on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ErrorRateStats {
    pub total: i64,
    pub errors: i64,
}

impl ErrorRateStats {
    pub fn error_rate(&self) -> f64 {
        if self.total <= 0 {
            0.0
        } else {
            self.errors as f64 / self.total as f64
        }
    }
}

/// Count requests / non-2xx rows in the last [`ALERT_WINDOW_MINUTES`] of
/// request_log. 2xx = success; everything else (401, 429, 499, 5xx, …) counts
/// as an error, matching the metrics `status_class` semantics.
async fn error_rate_counts(db: &Db) -> Result<(i64, i64), sqlx::Error> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS total, \
                COALESCE(SUM(CASE WHEN status >= 200 AND status < 300 THEN 0 ELSE 1 END), 0) AS errors \
         FROM request_log \
         WHERE created_at >= datetime('now', '-' || ? || ' minutes')",
    )
    .bind(ALERT_WINDOW_MINUTES)
    .fetch_one(db.pool())
    .await?;
    let total: i64 = row.try_get("total")?;
    let errors: i64 = row.try_get("errors")?;
    Ok((total, errors))
}

/// Compute the alert stats for the window, or `None` when the rate is below
/// the threshold (or the query fails — a DB outage is not an error-rate alarm).
async fn check_error_rate(db: &Db) -> Option<ErrorRateStats> {
    let (total, errors) = match error_rate_counts(db).await {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!(error = %e, "error-rate window query failed");
            return None;
        }
    };
    let stats = ErrorRateStats { total, errors };
    (stats.total >= ALERT_MIN_TOTAL && stats.error_rate() > ALERT_ERROR_RATIO).then_some(stats)
}

/// Fire-and-forget webhook POST when `ADMIN_ALERT_URL` is set: JSON body
/// `{errorRate, total, errors, ts}` with a 5s client timeout. The
/// `tracing::error!` in `run_maintenance_once` already fired; the webhook is
/// optional extra signal, so every failure here is only a WARN.
fn fire_alert(stats: ErrorRateStats) {
    let Some(url) = std::env::var("ADMIN_ALERT_URL")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let body = serde_json::json!({
        "errorRate": stats.error_rate(),
        "total": stats.total,
        "errors": stats.errors,
        "ts": ts,
    });
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "alert webhook client build failed");
                return;
            }
        };
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                tracing::warn!(
                    status = resp.status().as_u16(),
                    "admin alert webhook rejected the payload"
                );
            }
            Err(e) => tracing::warn!(error = %e, "admin alert webhook POST failed"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_providers::{ExaClient, FirecrawlClient, TavilyClient, XaiClient};

    /// Serializes process-env mutation so parallel tests never race set/remove.
    static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Serializes global-subscriber swaps (`set_default`) against other tests
    /// that capture tracing output.
    static CAPTURE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Test-only capture sink for WARN+ events (Arc-owned buffer, no leak).
    #[derive(Clone, Default)]
    struct CaptureSink(Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_warns(f: impl FnOnce()) -> String {
        capture_at(tracing::Level::WARN, f)
    }

    fn capture_at(level: tracing::Level, f: impl FnOnce()) -> String {
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(level)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let guard = sink.0.lock();
        String::from_utf8_lossy(&guard).into_owned()
    }

    /// Seed `n` request_log rows with one HTTP status (all inside the alert
    /// window — `created_at` defaults to `datetime('now')`).
    async fn seed_request_log(db: &Db, status: i64, n: usize) {
        for _ in 0..n {
            db.insert_request_log(
                "/api/search",
                "POST",
                status,
                Some("tavily"),
                Some("tavily"),
                Some(1),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("seed request_log row");
        }
    }

    fn providers_refused() -> serpotter_providers::ProviderRegistry {
        ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        )
    }

    #[test]
    fn invalid_cron_env_warns_and_defaults() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("REQUEST_LOG_RETENTION_DAYS", "not-a-number");
        let text = capture_warns(|| {
            assert_eq!(env_i64_or("REQUEST_LOG_RETENTION_DAYS", 30), 30);
        });
        std::env::remove_var("REQUEST_LOG_RETENTION_DAYS");
        assert!(
            text.contains("REQUEST_LOG_RETENTION_DAYS"),
            "warn must name the var: {text}"
        );
        assert!(
            text.contains("not-a-number"),
            "warn must carry the raw offending value: {text}"
        );
    }

    #[test]
    fn missing_cron_env_defaults_without_warning() {
        let text = capture_warns(|| {
            assert_eq!(env_i64_or("SERPOTTER_TEST_UNSET_VAR", 42), 42);
        });
        assert!(
            text.is_empty(),
            "no warn expected for a missing var: {text}"
        );
    }

    async fn count_admin_sessions(db: &Db) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM admin_sessions")
            .fetch_one(db.pool())
            .await
            .expect("count admin_sessions")
    }

    /// F55: the maintenance loop must NOT run at boot. With the boot-time
    /// immediate tick consumed, an expired admin_session inserted before spawn
    /// survives the first milliseconds and is purged only after one full period.
    #[tokio::test]
    async fn maintenance_first_tick_is_consumed_not_immediate() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let user = db
            .insert_admin_user("admin", "$argon2id$placeholder")
            .await
            .expect("insert admin user");
        db.insert_admin_session("boot-stale", user.id, "2000-01-01 00:00:00")
            .await
            .expect("insert stale session");
        let providers = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let handle =
            spawn_maintenance_with_period(db.clone(), providers, Duration::from_millis(60));

        // Well before the first period: no maintenance pass has run.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            count_admin_sessions(&db).await,
            1,
            "maintenance must not run at boot (immediate tick consumed)"
        );

        // After one full period the first real tick purges the stale row.
        let mut purged = false;
        for _ in 0..80 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if count_admin_sessions(&db).await == 0 {
                purged = true;
                break;
            }
        }
        assert!(purged, "first period must run the maintenance pass");
        handle.abort();
    }

    // --- B15: high-error-rate alerting ---------------------------------------

    #[tokio::test]
    async fn error_rate_above_threshold_triggers_alert() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        // 10 ok + 20 errors over 30 requests → ratio 0.667 > 0.5 and total >= 20.
        seed_request_log(&db, 200, 10).await;
        seed_request_log(&db, 500, 20).await;
        let stats = check_error_rate(&db).await.expect("alert must fire");
        assert_eq!(stats.total, 30);
        assert_eq!(stats.errors, 20);
        assert!((stats.error_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn error_rate_below_threshold_stays_silent() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        // 20 ok + 10 errors → ratio 1/3, no alert.
        seed_request_log(&db, 200, 20).await;
        seed_request_log(&db, 500, 10).await;
        assert!(
            check_error_rate(&db).await.is_none(),
            "no alert below ratio"
        );
    }

    #[tokio::test]
    async fn error_rate_respects_min_total_gate() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        // 5 requests, ALL errors: ratio 1.0 but total < 20 → no alert.
        seed_request_log(&db, 500, 5).await;
        assert!(
            check_error_rate(&db).await.is_none(),
            "tiny noisy sample must not alert"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // CAPTURE_LOCK deliberately serializes the whole capture window
    async fn alert_fires_tracing_error_when_triggered() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        seed_request_log(&db, 200, 10).await;
        seed_request_log(&db, 503, 20).await;

        // Global subscriber swap serialized against parallel capture tests.
        let _capture_guard = CAPTURE_LOCK.lock();
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        // Drive the real alert step (ADMIN_ALERT_URL unset → log only).
        alert_if_high_error_rate(&db).await;
        drop(_guard);
        let text = String::from_utf8_lossy(&sink.0.lock()).into_owned();
        assert!(
            text.contains("high request error rate"),
            "error! must fire above threshold: {text}"
        );
        assert!(
            text.contains("error_rate=0.666"),
            "carries the ratio: {text}"
        );
    }

    /// One-shot loopback HTTP server that captures the alert POST body.
    fn spawn_alert_listener() -> (String, std::sync::mpsc::Receiver<serde_json::Value>) {
        use std::io::{Read, Write};
        let (tx, rx) = std::sync::mpsc::channel();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let mut read = 0;
            // Read headers first (ends at \r\n\r\n).
            while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") && read < buf.len() {
                match stream.read(&mut buf[read..]) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => read += n,
                }
            }
            let head_end = buf[..read]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(read);
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let content_length: usize = head
                .lines()
                // reqwest sends lowercase header names; match case-insensitively.
                .find_map(|l| {
                    let lower = l.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            // Read the full body (headers + body already buffered, top up if short).
            while read < head_end + content_length && read < buf.len() {
                match stream.read(&mut buf[read..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => read += n,
                }
            }
            let body = &buf[head_end..head_end + content_length.min(read - head_end)];
            let _ = tx.send(serde_json::from_slice(body).unwrap_or_default());
            let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
        });
        (format!("http://{addr}"), rx)
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // ENV_LOCK deliberately serializes env mutation across the test
    async fn fire_alert_posts_json_to_webhook() {
        let _guard = ENV_LOCK.lock();
        let (url, rx) = spawn_alert_listener();
        std::env::set_var("ADMIN_ALERT_URL", url);

        fire_alert(ErrorRateStats {
            total: 40,
            errors: 30,
        });

        // The POST is fire-and-forget: poll until the loopback server replies.
        let mut received = None;
        for _ in 0..100 {
            match rx.try_recv() {
                Ok(v) => {
                    received = Some(v);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(_) => break,
            }
        }
        std::env::remove_var("ADMIN_ALERT_URL");
        drop(_guard);

        let v = received.expect("alert webhook must receive the payload");
        assert!((v["errorRate"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(v["total"], 40);
        assert_eq!(v["errors"], 30);
        assert!(
            v["ts"].as_i64().unwrap() > 1_600_000_000,
            "ts is unix seconds"
        );
    }

    #[tokio::test]
    async fn fire_alert_without_url_does_not_hang_or_panic() {
        let _guard = ENV_LOCK.lock();
        std::env::remove_var("ADMIN_ALERT_URL");
        // Must return synchronously (no task, no network attempt).
        fire_alert(ErrorRateStats {
            total: 30,
            errors: 20,
        });
    }

    // --- B16: expired provider_jobs purge ------------------------------------

    #[tokio::test]
    async fn maintenance_purges_expired_jobs() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let row = db
            .create_job("cccccccccccccccc", "tavily_research", "tavily", "{}", 3600)
            .await
            .expect("create job");
        assert_eq!(row.status, "running");
        // Force the expiry into the past so the next purge is deterministic
        // regardless of I1's TTL handling.
        sqlx::query("UPDATE provider_jobs SET expires_at = '2000-01-01 00:00:00' WHERE id = ?")
            .bind("cccccccccccccccc")
            .execute(db.pool())
            .await
            .expect("force expiry");

        run_maintenance_once(&db, &providers_refused()).await;

        assert!(
            db.get_job("cccccccccccccccc").await.unwrap().is_none(),
            "expired job must be purged by the maintenance pass"
        );
    }

    #[tokio::test]
    async fn maintenance_keeps_running_jobs() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        db.create_job("dddddddddddddddd", "tavily_research", "tavily", "{}", 3600)
            .await
            .expect("create job");

        run_maintenance_once(&db, &providers_refused()).await;

        assert!(
            db.get_job("dddddddddddddddd").await.unwrap().is_some(),
            "unexpired running job survives the purge"
        );
    }
}
