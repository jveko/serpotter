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
/// are None for rows that never resolved a key (e.g. early 401s) — SQLite
/// stores those with the sentinel `key_id=0`/`token_name=''`, mapped back
/// here so the wire shape is unchanged.
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
    /// Accumulate one request's usage into `usage_daily` for TODAY (UTC —
    /// `date('now')` in SQL). `key_id`/`token_name` use the sentinel `0`/`''`
    /// when the request never resolved a key/token (SQLite UNIQUE treats
    /// NULLs as distinct, so sentinels keep the conflict-dedupe honest).
    /// Additive — call once per completed request with per-request deltas.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_usage_daily(
        &self,
        service: &str,
        provider_used: &str,
        key_id: i64,
        token_name: &str,
        requests: i64,
        successes: i64,
        errors: i64,
        tokens: i64,
        cost: f64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO usage_daily (service, provider_used, date, key_id, token_name, requests, successes, errors, tokens, cost) \
             VALUES (?, ?, date('now'), ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(service, provider_used, date, key_id, token_name) DO UPDATE SET \
               requests = usage_daily.requests + excluded.requests, \
               successes = usage_daily.successes + excluded.successes, \
               errors = usage_daily.errors + excluded.errors, \
               tokens = usage_daily.tokens + excluded.tokens, \
               cost = usage_daily.cost + excluded.cost",
        )
        .bind(service)
        .bind(provider_used)
        .bind(key_id)
        .bind(token_name)
        .bind(requests)
        .bind(successes)
        .bind(errors)
        .bind(tokens)
        .bind(cost)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `usage_daily` rows for the last `days` days aggregated across
    /// key/token dims (one row per service+provider+date), newest first
    /// (`days` clamped 1..=90).
    pub async fn usage_summary(&self, days: i64) -> Result<Vec<UsageDailyRow>, DbError> {
        let days = days.clamp(1, 90);
        let rows = sqlx::query(
            "SELECT service, provider_used, date, \
                    SUM(requests) AS requests, SUM(successes) AS successes, \
                    SUM(errors) AS errors, SUM(tokens) AS tokens, SUM(cost) AS cost \
             FROM usage_daily \
             WHERE date >= date('now', '-' || ? || ' days') \
             GROUP BY service, provider_used, date \
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

    /// Aggregated spend per key/token from `usage_daily`, cost DESC. Sentinel
    /// `key_id=0`/`token_name=''` rows map to `None` (never-resolved keys).
    /// Used by `/api/spend/keys`.
    pub async fn spend_by_key(&self) -> Result<Vec<SpendKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT ud.key_id, ud.token_name, COALESCE(MAX(k.service), 'unknown') AS service, \
                    SUM(ud.requests) AS requests, SUM(ud.cost) AS cost \
             FROM usage_daily ud LEFT JOIN api_keys k ON k.id = ud.key_id \
             GROUP BY ud.key_id, ud.token_name \
             ORDER BY cost DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let key_id: i64 = r.try_get("key_id")?;
            let token_name: String = r.try_get("token_name")?;
            out.push(SpendKeyRow {
                key_id: (key_id != 0).then_some(key_id),
                token_name: (!token_name.is_empty()).then_some(token_name),
                service: r.try_get("service")?,
                requests: r.try_get("requests")?,
                cost: r.try_get("cost")?,
            });
        }
        Ok(out)
    }

    /// Aggregated spend per service from `usage_daily`, cost DESC.
    /// Used by `/api/spend/services`.
    pub async fn spend_by_service(&self) -> Result<Vec<SpendServiceRow>, DbError> {
        let rows = sqlx::query(
            "SELECT service, SUM(requests) AS requests, SUM(cost) AS cost \
             FROM usage_daily \
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

    async fn db() -> Db {
        Db::connect_for_test().await
    }

    #[tokio::test]
    async fn upsert_usage_daily_accumulates_same_key() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 120, 2.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 2, 1, 1, 40, 0.5)
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
    async fn upsert_usage_daily_key_dim_is_distinct() {
        let db = db().await;
        let k1 = db.insert_api_key("tavily", "tvly-1").await.unwrap();
        let k2 = db.insert_api_key("tavily", "tvly-2").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k1.id, "tok-1", 1, 1, 0, 0, 1.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", k2.id, "tok-2", 1, 1, 0, 0, 2.0)
            .await
            .unwrap();
        // Aggregated summary: one service/provider/date row, both keys summed.
        let rows = db.usage_summary(7).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requests, 2);
        assert!((rows[0].cost - 3.0).abs() < 1e-9);
        // Per-key spend keeps them separate.
        let by_key = db.spend_by_key().await.unwrap();
        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key[0].token_name.as_deref(), Some("tok-2"));
        assert!((by_key[0].cost - 2.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn usage_summary_filters_by_day_window() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 0, 0.0)
            .await
            .unwrap();
        // Backdate the row to 5 days ago (relative: no UTC-midnight flake).
        sqlx::query("UPDATE usage_daily SET date = date('now', '-5 days')")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.usage_summary(2).await.unwrap().is_empty());
        let wide = db.usage_summary(90).await.unwrap();
        assert_eq!(wide.len(), 1);
        assert_eq!(wide[0].service, "tavily");
    }

    #[tokio::test]
    async fn spend_aggregations_group_and_order() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 0, 3.0)
            .await
            .unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 0, 1, 0, 2.0)
            .await
            .unwrap();
        // Unknown-key row (sentinel) — cost with no resolved key.
        db.upsert_usage_daily("firecrawl", "firecrawl", 0, "tok-b", 1, 0, 1, 0, 1.0)
            .await
            .unwrap();

        let by_key = db.spend_by_key().await.unwrap();
        assert_eq!(by_key.len(), 2);
        assert_eq!(by_key[0].token_name.as_deref(), Some("tok-a"));
        assert!(by_key[0].key_id.is_some());
        assert_eq!(by_key[0].service, "tavily");
        assert_eq!(by_key[0].requests, 2);
        assert!((by_key[0].cost - 5.0).abs() < 1e-9);
        assert_eq!(by_key[1].token_name.as_deref(), Some("tok-b"));
        assert!(by_key[1].key_id.is_none(), "sentinel 0 maps to null");
        assert_eq!(by_key[1].service, "unknown", "no api_keys row for key_id 0");
        assert!((by_key[1].cost - 1.0).abs() < 1e-9);

        let by_service = db.spend_by_service().await.unwrap();
        assert_eq!(by_service.len(), 2);
        assert_eq!(by_service[0].service, "tavily");
        assert_eq!(by_service[0].requests, 2);
        assert!((by_service[0].cost - 5.0).abs() < 1e-9);
    }
}
