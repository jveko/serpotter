use crate::{Db, DbError};
use sqlx::Row;

/// One `usage_daily` row (B6 usage dashboard source).
#[derive(Clone, Debug, PartialEq)]
pub struct UsageDailyRow {
    pub service: String,
    pub provider_used: String,
    pub date: String,
    pub requests: i64,
    pub successes: i64,
    pub errors: i64,
    pub tokens: i64,
    pub cost: f64,
}

/// Aggregated spend per key/token (`/api/spend/keys`). `key_id`/`token_name`
/// are NULL for rows that never resolved a key (e.g. early 401s).
#[derive(Clone, Debug, PartialEq)]
pub struct SpendKeyRow {
    pub key_id: Option<i64>,
    pub token_name: Option<String>,
    pub service: String,
    pub requests: i64,
    pub cost: f64,
}

/// Aggregated spend per service (`/api/spend/services`).
#[derive(Clone, Debug, PartialEq)]
pub struct SpendServiceRow {
    pub service: String,
    pub requests: i64,
    pub cost: f64,
}

impl Db {
    /// Accumulate one request's usage into `usage_daily` (additive — call once
    /// per completed request with per-request deltas). `date` is 'YYYY-MM-DD'.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_usage_daily(
        &self,
        service: &str,
        provider_used: &str,
        date: &str,
        requests: i64,
        successes: i64,
        errors: i64,
        tokens: i64,
        cost: f64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO usage_daily (service, provider_used, date, requests, successes, errors, tokens, cost) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(service, provider_used, date) DO UPDATE SET \
               requests = usage_daily.requests + excluded.requests, \
               successes = usage_daily.successes + excluded.successes, \
               errors = usage_daily.errors + excluded.errors, \
               tokens = usage_daily.tokens + excluded.tokens, \
               cost = usage_daily.cost + excluded.cost",
        )
        .bind(service)
        .bind(provider_used)
        .bind(date)
        .bind(requests)
        .bind(successes)
        .bind(errors)
        .bind(tokens)
        .bind(cost)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Idempotently recompute `usage_daily` from `request_log` rows newer than
    /// `since_hours` (status 2xx = success, anything else = error; tokens/cost
    /// from the B2 columns). Replaces, does not accumulate — safe to re-run
    /// over the same window without double counting. Returns rows upserted.
    pub async fn rollup_usage_from_request_log(&self, since_hours: i64) -> Result<u64, DbError> {
        let since = since_hours.max(0);
        let agg = sqlx::query(
            "SELECT COALESCE(service, 'unknown') AS service, \
                    COALESCE(provider_used, 'unknown') AS provider_used, \
                    date(created_at) AS day, \
                    COUNT(*) AS requests, \
                    SUM(CASE WHEN status >= 200 AND status < 300 THEN 1 ELSE 0 END) AS successes, \
                    SUM(CASE WHEN status >= 200 AND status < 300 THEN 0 ELSE 1 END) AS errors, \
                    COALESCE(SUM(total_tokens), 0) AS tokens, \
                    COALESCE(SUM(cost_est), 0.0) AS cost \
             FROM request_log \
             WHERE created_at >= datetime('now', '-' || ? || ' hours') \
             GROUP BY COALESCE(service, 'unknown'), COALESCE(provider_used, 'unknown'), date(created_at)",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?;

        let mut written = 0u64;
        for r in agg {
            let service: String = r.try_get("service")?;
            let provider_used: String = r.try_get("provider_used")?;
            let day: String = r.try_get("day")?;
            let requests: i64 = r.try_get("requests")?;
            let successes: i64 = r.try_get("successes")?;
            let errors: i64 = r.try_get("errors")?;
            let tokens: i64 = r.try_get("tokens")?;
            let cost: f64 = r.try_get("cost")?;
            let up = sqlx::query(
                "INSERT INTO usage_daily (service, provider_used, date, requests, successes, errors, tokens, cost) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(service, provider_used, date) DO UPDATE SET \
                   requests = excluded.requests, \
                   successes = excluded.successes, \
                   errors = excluded.errors, \
                   tokens = excluded.tokens, \
                   cost = excluded.cost",
            )
            .bind(&service)
            .bind(&provider_used)
            .bind(&day)
            .bind(requests)
            .bind(successes)
            .bind(errors)
            .bind(tokens)
            .bind(cost)
            .execute(&self.pool)
            .await?;
            written += up.rows_affected();
        }
        Ok(written)
    }

    /// `usage_daily` rows for the last `days` days, newest first
    /// (`days` clamped 1..=90).
    pub async fn usage_summary(&self, days: i64) -> Result<Vec<UsageDailyRow>, DbError> {
        let days = days.clamp(1, 90);
        let rows = sqlx::query(
            "SELECT service, provider_used, date, requests, successes, errors, tokens, cost \
             FROM usage_daily \
             WHERE date >= date('now', '-' || ? || ' days') \
             ORDER BY date DESC, service ASC, provider_used ASC",
        )
        .bind(days)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(UsageDailyRow {
                service: r.try_get("service")?,
                provider_used: r.try_get("provider_used")?,
                date: r.try_get("date")?,
                requests: r.try_get("requests")?,
                successes: r.try_get("successes")?,
                errors: r.try_get("errors")?,
                tokens: r.try_get("tokens")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }

    /// Aggregated spend per key/token from `request_log.cost_est`, cost DESC.
    /// Used by `/api/spend/keys` (raw GROUP BY lives here — the api crate
    /// must not depend on sqlx at runtime).
    pub async fn spend_by_key(&self) -> Result<Vec<SpendKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT rl.key_id, rl.token_name, COALESCE(MAX(k.service), 'unknown') AS service, \
                    COUNT(*) AS requests, COALESCE(SUM(rl.cost_est), 0) AS cost \
             FROM request_log rl LEFT JOIN api_keys k ON k.id = rl.key_id \
             GROUP BY rl.key_id, rl.token_name \
             ORDER BY cost DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(SpendKeyRow {
                key_id: r.try_get("key_id")?,
                token_name: r.try_get("token_name")?,
                service: r.try_get("service")?,
                requests: r.try_get("requests")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }

    /// Aggregated spend per service from `request_log.cost_est`, cost DESC.
    /// Used by `/api/spend/services`.
    pub async fn spend_by_service(&self) -> Result<Vec<SpendServiceRow>, DbError> {
        let rows = sqlx::query(
            "SELECT COALESCE(service, 'unknown') AS service, COUNT(*) AS requests, \
                    COALESCE(SUM(cost_est), 0.0) AS cost \
             FROM request_log \
             GROUP BY service \
             ORDER BY cost DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(SpendServiceRow {
                service: r.try_get("service")?,
                requests: r.try_get("requests")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RequestLogFilter, RequestLogRow};

    async fn db() -> Db {
        Db::connect_for_test().await
    }

    #[tokio::test]
    async fn upsert_usage_daily_accumulates() {
        let db = db().await;
        db.upsert_usage_daily("tavily", "tavily", "2026-08-11", 1, 1, 0, 120, 2.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", "2026-08-11", 2, 1, 1, 40, 0.5)
            .await
            .unwrap();
        let rows = db.usage_summary(7).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 3);
        assert_eq!(rows[0].successes, 2);
        assert_eq!(rows[0].errors, 1);
        assert_eq!(rows[0].tokens, 160);
        assert!((rows[0].cost - 2.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn usage_summary_filters_by_day_window() {
        let db = db().await;
        db.upsert_usage_daily("tavily", "tavily", "2026-08-11", 1, 1, 0, 0, 0.0)
            .await
            .unwrap();
        // Old row (5 days back today) only shows in wide windows.
        let old = sqlx::query("SELECT date('now', '-5 days') AS d")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let old_day: String = old.try_get("d").unwrap();
        db.upsert_usage_daily("exa", "exa", &old_day, 4, 4, 0, 0, 0.0)
            .await
            .unwrap();
        let near = db.usage_summary(2).await.unwrap();
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].service, "tavily");
        let wide = db.usage_summary(90).await.unwrap();
        assert_eq!(wide.len(), 2);
    }

    #[tokio::test]
    async fn rollup_from_request_log_is_correct_and_idempotent() {
        let db = db().await;
        // 2 success + 1 error rows with tokens/cost (B2 columns via full insert).
        db.insert_request_log_full(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
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
            Some(10),
            Some(20),
            Some(30),
            Some(1.5),
        )
        .await
        .unwrap();
        db.insert_request_log_full(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
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
            Some(5),
            Some(5),
            Some(10),
            Some(0.5),
        )
        .await
        .unwrap();
        db.insert_request_log_full(
            "/api/extract",
            "POST",
            502,
            Some("tavily"),
            Some("tavily"),
            None,
            Some("provider"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(0),
            Some(0),
            Some(0),
            Some(0.0),
        )
        .await
        .unwrap();

        let written = db.rollup_usage_from_request_log(24).await.unwrap();
        assert_eq!(
            written, 1,
            "one (service, provider, date) group in the window"
        );

        let rows = db.usage_summary(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].service, "tavily");
        assert_eq!(rows[0].requests, 3);
        assert_eq!(rows[0].successes, 2);
        assert_eq!(rows[0].errors, 1);
        assert_eq!(rows[0].tokens, 40);
        assert!((rows[0].cost - 2.0).abs() < 1e-9);

        // Idempotent: re-rolling the same window replaces, not doubles.
        db.rollup_usage_from_request_log(24).await.unwrap();
        let again = db.usage_summary(1).await.unwrap();
        assert_eq!(again[0].requests, 3);
        assert_eq!(again[0].successes, 2);
        assert_eq!(again[0].cost, 2.0);
    }

    #[tokio::test]
    async fn rollup_outside_window_is_ignored() {
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
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        // Backdate the row to 2 days ago; a 24h rollup must skip it.
        sqlx::query("UPDATE request_log SET created_at = datetime('now', '-2 days')")
            .execute(db.pool())
            .await
            .unwrap();
        let written = db.rollup_usage_from_request_log(24).await.unwrap();
        assert_eq!(written, 0);
        assert!(db.usage_summary(1).await.unwrap().is_empty());
        // Wide window still sees it.
        let written = db.rollup_usage_from_request_log(24 * 7).await.unwrap();
        assert_eq!(written, 1);
    }

    #[tokio::test]
    async fn spend_aggregations_group_and_order() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.insert_request_log_full(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            Some("tavily"),
            None,
            None,
            None,
            None,
            Some("tok-a"),
            None,
            None,
            None,
            Some(k.id),
            None,
            None,
            None,
            None,
            Some(3.0),
        )
        .await
        .unwrap();
        db.insert_request_log_full(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            Some("tavily"),
            None,
            None,
            None,
            None,
            Some("tok-a"),
            None,
            None,
            None,
            Some(k.id),
            None,
            None,
            None,
            None,
            Some(2.0),
        )
        .await
        .unwrap();
        db.insert_request_log_full(
            "/api/extract",
            "POST",
            502,
            Some("firecrawl"),
            Some("firecrawl"),
            None,
            Some("provider"),
            None,
            None,
            Some("tok-b"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(1.0),
        )
        .await
        .unwrap();

        let by_key = db.spend_by_key().await.unwrap();
        // tok-a (5.0 cost, key resolved → service tavily) first, then tok-b.
        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key[0].token_name.as_deref(), Some("tok-a"));
        assert!(by_key[0].key_id.is_some());
        assert_eq!(by_key[0].service, "tavily");
        assert_eq!(by_key[0].requests, 2);
        assert!((by_key[0].cost - 5.0).abs() < 1e-9);
        assert_eq!(by_key[1].token_name.as_deref(), Some("tok-b"));
        // No key_id row → service falls back to 'unknown' (from the key join).
        assert_eq!(by_key[1].service, "unknown");
        assert!(by_key[1].key_id.is_none());
        assert!((by_key[1].cost - 1.0).abs() < 1e-9);

        let by_service = db.spend_by_service().await.unwrap();
        assert_eq!(by_service.len(), 2);
        assert_eq!(by_service[0].service, "tavily");
        assert_eq!(by_service[0].requests, 2);
        assert!((by_service[0].cost - 5.0).abs() < 1e-9);
    }

    // Re-exported row types stay constructible/cloneable for admin handlers.
    #[allow(dead_code)]
    fn row_shapes(_: &Db) {
        let _ = RequestLogRow {
            id: 1,
            created_at: String::new(),
            path: String::new(),
            method: String::new(),
            status: 200,
            service: None,
            provider_used: None,
            duration_ms: None,
            error_kind: None,
            query_preview: None,
            request_id: None,
            token_name: None,
            strategy: None,
            providers_consulted: None,
            attempt_count: None,
            key_id: None,
            node_id: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost_est: None,
        };
        let _ = RequestLogFilter::default();
        let _ = UsageDailyRow {
            service: String::new(),
            provider_used: String::new(),
            date: String::new(),
            requests: 0,
            successes: 0,
            errors: 0,
            tokens: 0,
            cost: 0.0,
        };
    }
}
