//! Background maintenance: re-enable stale keys, purge request_log, optional credit sync.

use std::sync::Arc;
use std::time::Duration;

use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;
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

/// Read an integer cron env var, warning (never silently) when the value is set
/// but unparseable. Missing var → `default` without a warning.
fn env_i64_or(key: &str, default: i64) -> i64 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_providers::{ExaClient, FirecrawlClient, TavilyClient, XaiClient};

    /// Serializes process-env mutation so parallel tests never race set/remove.
    static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

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
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let guard = sink.0.lock();
        String::from_utf8_lossy(&guard).into_owned()
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
}
