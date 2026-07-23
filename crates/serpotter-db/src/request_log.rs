use crate::{Db, DbError};
use sqlx::Row;


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestLogRow {
    pub id: i64,
    pub created_at: String,
    pub path: String,
    pub method: String,
    pub status: i64,
    pub service: Option<String>,
    pub provider_used: Option<String>,
    pub duration_ms: Option<i64>,
    pub error_kind: Option<String>,
    pub query_preview: Option<String>,
}

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

    /// Newest-first request log page for admin browser (limit clamped 1..=200).
    pub async fn list_request_logs(&self, limit: i64) -> Result<Vec<RequestLogRow>, DbError> {
        let limit = limit.clamp(1, 200);
        let rows = sqlx::query(
            "SELECT id, created_at, path, method, status, service, provider_used, \
                    duration_ms, error_kind, query_preview \
             FROM request_log \
             ORDER BY created_at DESC, id DESC \
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(RequestLogRow {
                id: r.try_get("id")?,
                created_at: r.try_get("created_at")?,
                path: r.try_get("path")?,
                method: r.try_get("method")?,
                status: r.try_get("status")?,
                service: r.try_get("service")?,
                provider_used: r.try_get("provider_used")?,
                duration_ms: r.try_get("duration_ms")?,
                error_kind: r.try_get("error_kind")?,
                query_preview: r.try_get("query_preview")?,
            });
        }
        Ok(out)
    }
}
