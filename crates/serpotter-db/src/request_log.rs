use crate::{Db, DbError};
use sqlx::Row;

impl Db {
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_request_log(
        &self,
        path: &str,
        method: &str,
        status: i64,
        service: Option<&str>,
        provider_used: Option<&str>,
        duration_ms: Option<i64>,
        error_kind: Option<&str>,
        query_preview: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO request_log \
             (path, method, status, service, provider_used, duration_ms, error_kind, query_preview) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(path)
        .bind(method)
        .bind(status)
        .bind(service)
        .bind(provider_used)
        .bind(duration_ms)
        .bind(error_kind)
        .bind(query_preview)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete logs older than `retention_days`, then cap total rows to `max_rows` (oldest first).
    pub async fn purge_request_log(
        &self,
        retention_days: i64,
        max_rows: i64,
    ) -> Result<u64, DbError> {
        let days = retention_days.max(0);
        let max_rows = max_rows.max(0);
        let aged = sqlx::query(
            "DELETE FROM request_log WHERE created_at < datetime('now', '-' || ? || ' days')",
        )
        .bind(days)
        .execute(&self.pool)
        .await?
        .rows_affected();

        let capped = if max_rows == 0 {
            sqlx::query("DELETE FROM request_log")
                .execute(&self.pool)
                .await?
                .rows_affected()
        } else {
            // Keep the newest max_rows; delete the rest (oldest first via OFFSET).
            // Nested SELECT so SQLite allows DELETE of the same table.
            sqlx::query(
                "DELETE FROM request_log WHERE id IN (
                    SELECT id FROM (
                        SELECT id FROM request_log
                        ORDER BY created_at ASC, id ASC
                        LIMIT -1 OFFSET ?
                    )
                )",
            )
            .bind(max_rows)
            .execute(&self.pool)
            .await?
            .rows_affected()
        };
        Ok(aged + capped)
    }

    pub async fn count_request_logs(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM request_log")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }
}
