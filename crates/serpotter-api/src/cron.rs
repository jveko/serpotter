//! Background maintenance: re-enable stale keys, purge request_log, optional credit sync.

use std::sync::Arc;
use std::time::Duration;

use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;
use tokio::task::JoinHandle;

/// Spawn a 15-minute interval loop for key re-enable, request_log purge,
/// and optional Tavily/Firecrawl credit sync when `CREDIT_SYNC_CRON=1`.
/// Returns a handle so the caller can abort the task on process shutdown.
pub fn spawn_maintenance(db: Db, providers: ProviderRegistry) -> JoinHandle<()> {
    let providers = Arc::new(providers);
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(900)); // 15m
        loop {
            tick.tick().await;
            let hours: i64 = std::env::var("KEY_REENABLE_AFTER_HOURS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(24);
            let days: i64 = std::env::var("REQUEST_LOG_RETENTION_DAYS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(30);
            let max_rows: i64 = std::env::var("REQUEST_LOG_MAX_ROWS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(100_000);
            match db.reenable_stale_keys(hours).await {
                Ok(n) if n > 0 => tracing::info!(n, hours, "re-enabled stale api keys"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "reenable_stale_keys failed"),
            }
            match db.purge_request_log(days, max_rows).await {
                Ok(n) if n > 0 => tracing::info!(n, days, max_rows, "purged request_log rows"),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "purge_request_log failed"),
            }

            // Off by default — avoid hammering vendor usage APIs every 15m.
            let credit_sync = std::env::var("CREDIT_SYNC_CRON")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            if credit_sync {
                match crate::credit_sync::sync_credits_for_services(
                    &db,
                    providers.as_ref(),
                    &["tavily", "firecrawl"],
                )
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
    })
}
