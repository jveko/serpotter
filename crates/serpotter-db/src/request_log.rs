use crate::{Db, DbError};
use sqlx::{QueryBuilder, Row, Sqlite};

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
    pub request_id: Option<String>,
    pub token_name: Option<String>,
    pub strategy: Option<String>,
    pub providers_consulted: Option<String>,
    pub attempt_count: Option<i64>,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
}

/// Admin list filters for request_log (newest-first).
#[derive(Clone, Debug, Default)]
pub struct RequestLogFilter {
    pub limit: i64,
    pub status: Option<i64>,
    pub path_prefix: Option<String>,
    pub service: Option<String>,
    pub request_id: Option<String>,
}

impl Db {
    /// Insert one request_log row. New observability columns are optional (NULL when unknown).
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
        request_id: Option<&str>,
        token_name: Option<&str>,
        strategy: Option<&str>,
        providers_consulted: Option<&str>,
        attempt_count: Option<i64>,
        key_id: Option<i64>,
        node_id: Option<i64>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO request_log \
             (path, method, status, service, provider_used, duration_ms, error_kind, query_preview, \
              request_id, token_name, strategy, providers_consulted, attempt_count, key_id, node_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(path)
        .bind(method)
        .bind(status)
        .bind(service)
        .bind(provider_used)
        .bind(duration_ms)
        .bind(error_kind)
        .bind(query_preview)
        .bind(request_id)
        .bind(token_name)
        .bind(strategy)
        .bind(providers_consulted)
        .bind(attempt_count)
        .bind(key_id)
        .bind(node_id)
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

    /// Newest-first request log page for admin browser.
    ///
    /// `path_prefix` matches `path LIKE prefix || '%'` (bound as one pattern).
    /// Limit is clamped to 1..=200.
    pub async fn list_request_logs(
        &self,
        filter: RequestLogFilter,
    ) -> Result<Vec<RequestLogRow>, DbError> {
        let limit = filter.limit.clamp(1, 200);
        let path_pat = filter.path_prefix.as_ref().map(|p| format!("{p}%"));

        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT id, created_at, path, method, status, service, provider_used, \
             duration_ms, error_kind, query_preview, request_id, token_name, strategy, \
             providers_consulted, attempt_count, key_id, node_id \
             FROM request_log WHERE 1=1",
        );
        if let Some(s) = filter.status {
            qb.push(" AND status = ");
            qb.push_bind(s);
        }
        if let Some(pat) = &path_pat {
            qb.push(" AND path LIKE ");
            qb.push_bind(pat);
        }
        if let Some(svc) = &filter.service {
            qb.push(" AND service = ");
            qb.push_bind(svc);
        }
        if let Some(rid) = &filter.request_id {
            qb.push(" AND request_id = ");
            qb.push_bind(rid);
        }
        qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        qb.push_bind(limit);

        let rows = qb.build().fetch_all(&self.pool).await?;
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
                request_id: r.try_get("request_id")?,
                token_name: r.try_get("token_name")?,
                strategy: r.try_get("strategy")?,
                providers_consulted: r.try_get("providers_consulted")?,
                attempt_count: r.try_get("attempt_count")?,
                key_id: r.try_get("key_id")?,
                node_id: r.try_get("node_id")?,
            });
        }
        Ok(out)
    }
}
