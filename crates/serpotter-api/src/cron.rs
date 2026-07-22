//! Background maintenance: re-enable stale keys + purge request_log.

use std::time::Duration;

use serpotter_db::Db;

/// Spawn a 15-minute interval loop for key re-enable and request_log purge.
pub fn spawn_maintenance(db: Db) {
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
        }
    });
}
