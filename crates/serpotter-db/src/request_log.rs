use crate::{Db, DbError};
use sqlx::{QueryBuilder, Row, Sqlite};

/// One `request_log` row. B2 columns (input/output/total tokens, cost_est,
/// ttft_ms, request_mode) are NULL when unknown (e.g. before the token/cost
/// capture wave, or on early 401s).
#[derive(Clone, Debug, PartialEq)]
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
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_est: Option<f64>,
    pub ttft_ms: Option<f64>,
    pub request_mode: Option<String>,
}

/// Admin list filters for request_log (newest-first).
#[derive(Clone, Debug, Default)]
pub struct RequestLogFilter {
    pub limit: i64,
    pub status: Option<i64>,
    pub path_prefix: Option<String>,
    pub service: Option<String>,
    pub request_id: Option<String>,
    /// 0-based page offset (B13 pagination).
    pub offset: i64,
    /// Exact token_name filter (B13).
    pub token_name: Option<String>,
}

impl Db {
    /// Insert one request_log row (B2 columns left NULL).
    ///
    /// Back-compat wrapper over [`Db::insert_request_log_full`] — keeps the
    /// pre-wave call sites (and their tests) compiling unchanged.
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
        self.insert_request_log_full(
            path,
            method,
            status,
            service,
            provider_used,
            duration_ms,
            error_kind,
            query_preview,
            request_id,
            token_name,
            strategy,
            providers_consulted,
            attempt_count,
            key_id,
            node_id,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Insert one request_log row including the B2 token/cost columns.
    /// `request_mode` is 'oneshot' | 'stream' | NULL=unknown; `ttft_ms` is the
    /// time-to-first-token in ms when the wave captures it.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_request_log_full(
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
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        total_tokens: Option<i64>,
        cost_est: Option<f64>,
        ttft_ms: Option<f64>,
        request_mode: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO request_log \
             (path, method, status, service, provider_used, duration_ms, error_kind, query_preview, \
              request_id, token_name, strategy, providers_consulted, attempt_count, key_id, node_id, \
              input_tokens, output_tokens, total_tokens, cost_est, ttft_ms, request_mode) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(total_tokens)
        .bind(cost_est)
        .bind(ttft_ms)
        .bind(request_mode)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete logs older than `retention_days`, then cap total rows to `max_rows`
    /// (keeps the NEWEST `max_rows`; deletes the oldest overflow).
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
            // Newest-first window: with identical created_at (sub-second bulk
            // inserts), id DESC breaks the tie so the newest rows are kept.
            sqlx::query(
                "DELETE FROM request_log WHERE id IN (
                    SELECT id FROM (
                        SELECT id FROM request_log
                        ORDER BY created_at DESC, id DESC
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
    /// `token_name` is an exact match. `limit` is clamped to 1..=200,
    /// `offset` to >= 0 (B13 pagination).
    pub async fn list_request_logs(
        &self,
        filter: RequestLogFilter,
    ) -> Result<Vec<RequestLogRow>, DbError> {
        let limit = filter.limit.clamp(1, 200);
        let offset = filter.offset.max(0);
        let path_pat = filter.path_prefix.as_ref().map(|p| format!("{p}%"));

        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new(
            "SELECT id, created_at, path, method, status, service, provider_used, \
             duration_ms, error_kind, query_preview, request_id, token_name, strategy, \
             providers_consulted, attempt_count, key_id, node_id, \
             input_tokens, output_tokens, total_tokens, cost_est, ttft_ms, request_mode \
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
        if let Some(tok) = &filter.token_name {
            qb.push(" AND token_name = ");
            qb.push_bind(tok);
        }
        qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        qb.push_bind(limit);
        qb.push(" OFFSET ");
        qb.push_bind(offset);

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
                input_tokens: r.try_get("input_tokens")?,
                output_tokens: r.try_get("output_tokens")?,
                total_tokens: r.try_get("total_tokens")?,
                cost_est: r.try_get("cost_est")?,
                ttft_ms: r.try_get("ttft_ms")?,
                request_mode: r.try_get("request_mode")?,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        Db::connect_for_test().await
    }

    #[tokio::test]
    async fn insert_full_roundtrips_b2_columns() {
        let db = db().await;
        db.insert_request_log_full(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            Some("tavily"),
            Some(12),
            None,
            Some("q"),
            Some("rid-1"),
            Some("tok-a"),
            Some("hybrid"),
            Some("tavily,firecrawl"),
            Some(2),
            Some(7),
            Some(3),
            Some(10),
            Some(20),
            Some(30),
            Some(1.5),
            Some(4.2),
            Some("oneshot"),
        )
        .await
        .unwrap();
        let rows = db
            .list_request_logs(RequestLogFilter::default())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.input_tokens, Some(10));
        assert_eq!(r.output_tokens, Some(20));
        assert_eq!(r.total_tokens, Some(30));
        assert_eq!(r.cost_est, Some(1.5));
        assert_eq!(r.ttft_ms, Some(4.2));
        assert_eq!(r.request_mode.as_deref(), Some("oneshot"));
    }

    #[tokio::test]
    async fn list_request_logs_pagination() {
        let db = db().await;
        for i in 0..5 {
            db.insert_request_log(
                "/api/search",
                "POST",
                200,
                Some("tavily"),
                None,
                None,
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
            .unwrap();
            if i < 4 {
                // Stagger created_at so ordering is deterministic.
                sqlx::query("UPDATE request_log SET created_at = datetime('now', '-' || ? || ' seconds') WHERE id = ?")
                    .bind(5 - i)
                    .bind(i + 1)
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
        }
        let page1 = db
            .list_request_logs(RequestLogFilter {
                limit: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].id, 5, "newest first");
        assert_eq!(page1[1].id, 4);
        let page2 = db
            .list_request_logs(RequestLogFilter {
                limit: 2,
                offset: 2,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].id, 3);
        assert_eq!(page2[1].id, 2);
        let page3 = db
            .list_request_logs(RequestLogFilter {
                limit: 2,
                offset: 4,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].id, 1);
        // Offset beyond the page → empty, not error.
        let past = db
            .list_request_logs(RequestLogFilter {
                limit: 2,
                offset: 99,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(past.is_empty());
    }

    #[tokio::test]
    async fn list_request_logs_token_name_filter() {
        let db = db().await;
        db.insert_request_log(
            "/api/search",
            "POST",
            200,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("tok-a"),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        db.insert_request_log(
            "/api/extract",
            "POST",
            200,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("tok-b"),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        let rows = db
            .list_request_logs(RequestLogFilter {
                token_name: Some("tok-b".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].token_name.as_deref(), Some("tok-b"));
        // Exact match: a prefix must not hit.
        let rows = db
            .list_request_logs(RequestLogFilter {
                token_name: Some("tok".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn request_log_v15_columns_nullable_for_legacy_inserts() {
        let db = db().await;
        // The back-compat wrapper leaves B2 columns NULL.
        db.insert_request_log(
            "/api/search",
            "POST",
            200,
            None,
            None,
            None,
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
        .unwrap();
        let rows = db
            .list_request_logs(RequestLogFilter::default())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, None);
        assert_eq!(rows[0].total_tokens, None);
        assert_eq!(rows[0].cost_est, None);
        assert_eq!(rows[0].ttft_ms, None);
        assert_eq!(rows[0].request_mode, None);
    }
}
