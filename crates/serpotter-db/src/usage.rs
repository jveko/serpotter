use crate::{Db, DbError};
use sqlx::Row;

/// Upper bound for any usage/spend `days` window, shared by the DB layer and
/// the API handlers so a requested window can never be silently truncated on
/// the way down. 180 days exists so the admin dashboard's current+previous
/// window pattern (`days * 2`) works at its 90d setting.
pub const USAGE_MAX_DAYS: i64 = 180;
/// Lower bound for any usage/spend `days` window (a zero/negative window would
/// match nothing at all).
pub const USAGE_MIN_DAYS: i64 = 1;
/// Row cap for the aggregated spend endpoints. `usage_daily` has no retention
/// job, so the top-spender list is bounded instead of scanning every key that
/// ever made a request.
pub const SPEND_MAX_ROWS: i64 = 500;

/// The single `days` clamp every usage/spend query shares.
pub fn clamp_usage_days(days: i64) -> i64 {
    days.clamp(USAGE_MIN_DAYS, USAGE_MAX_DAYS)
}
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
    /// key/token dims (one row per service+provider+date), newest first.
    /// `days` goes through the shared [`clamp_usage_days`] (1..=USAGE_MAX_DAYS).
    pub async fn usage_summary(&self, days: i64) -> Result<Vec<UsageDailyRow>, DbError> {
        let days = clamp_usage_days(days);
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

    /// Aggregated spend per key/token from `usage_daily` over a `days` window,
    /// cost DESC, capped at [`SPEND_MAX_ROWS`] (top spenders first). Sentinel
    /// `key_id=0`/`token_name=''` rows map to `None` (never-resolved keys).
    /// Used by `/api/spend/keys`.
    pub async fn spend_by_key(&self, days: i64) -> Result<Vec<SpendKeyRow>, DbError> {
        let days = clamp_usage_days(days);
        let rows = sqlx::query(
            "SELECT ud.key_id, ud.token_name, COALESCE(MAX(k.service), 'unknown') AS service, \
                    SUM(ud.requests) AS requests, SUM(ud.cost) AS cost \
             FROM usage_daily ud LEFT JOIN api_keys k ON k.id = ud.key_id \
             WHERE ud.date >= date('now', '-' || ? || ' days') \
             GROUP BY ud.key_id, ud.token_name \
             ORDER BY cost DESC \
             LIMIT ?",
        )
        .bind(days)
        .bind(SPEND_MAX_ROWS)
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

    /// Aggregated spend per service from `usage_daily` over a `days` window,
    /// cost DESC, capped at [`SPEND_MAX_ROWS`]. Used by `/api/spend/services`.
    pub async fn spend_by_service(&self, days: i64) -> Result<Vec<SpendServiceRow>, DbError> {
        let days = clamp_usage_days(days);
        let rows = sqlx::query(
            "SELECT service, SUM(requests) AS requests, SUM(cost) AS cost \
             FROM usage_daily \
             WHERE date >= date('now', '-' || ? || ' days') \
             GROUP BY service \
             ORDER BY cost DESC \
             LIMIT ?",
        )
        .bind(days)
        .bind(SPEND_MAX_ROWS)
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
        let by_key = db.spend_by_key(7).await.unwrap();
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

    /// The 90-day truncation bug: a request for a wider window must actually
    /// reach past day 90 (the admin dashboard fetches `2×days`).
    #[tokio::test]
    async fn usage_summary_window_reaches_past_ninety_days() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-a", 1, 1, 0, 0, 0.0)
            .await
            .unwrap();
        // Backdate past the old 90-day ceiling (relative: no UTC-midnight flake).
        sqlx::query("UPDATE usage_daily SET date = date('now', '-120 days')")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            db.usage_summary(90).await.unwrap().is_empty(),
            "a 90-day window must not reach 120 days back"
        );
        let wide = db.usage_summary(USAGE_MAX_DAYS).await.unwrap();
        assert_eq!(wide.len(), 1, "the 180-day window must include day 120");
        // Above the shared bound the window clamps instead of reaching further.
        let clamped = db.usage_summary(3650).await.unwrap();
        assert_eq!(clamped, wide, "days above the bound clamp to the bound");
    }

    #[tokio::test]
    async fn clamp_usage_days_is_the_single_shared_bound() {
        assert_eq!(clamp_usage_days(0), USAGE_MIN_DAYS);
        assert_eq!(clamp_usage_days(-30), USAGE_MIN_DAYS);
        assert_eq!(clamp_usage_days(14), 14);
        assert_eq!(clamp_usage_days(USAGE_MAX_DAYS), USAGE_MAX_DAYS);
        assert_eq!(clamp_usage_days(USAGE_MAX_DAYS + 1), USAGE_MAX_DAYS);
    }

    /// A row older than the requested window must not appear in the spend
    /// aggregates (the full-table version returned it for any `days`).
    #[tokio::test]
    async fn spend_aggregations_respect_the_day_window() {
        let db = db().await;
        let k = db.insert_api_key("tavily", "tvly-key").await.unwrap();
        db.upsert_usage_daily("tavily", "tavily", k.id, "tok-old", 1, 1, 0, 0, 9.0)
            .await
            .unwrap();
        sqlx::query("UPDATE usage_daily SET date = date('now', '-40 days')")
            .execute(db.pool())
            .await
            .unwrap();
        db.upsert_usage_daily("firecrawl", "firecrawl", 0, "tok-new", 1, 1, 0, 0, 1.0)
            .await
            .unwrap();

        let by_key = db.spend_by_key(7).await.unwrap();
        assert_eq!(
            by_key.len(),
            1,
            "40-day-old spend is outside a 7-day window"
        );
        assert_eq!(by_key[0].token_name.as_deref(), Some("tok-new"));
        let by_service = db.spend_by_service(7).await.unwrap();
        assert_eq!(by_service.len(), 1);
        assert_eq!(by_service[0].service, "firecrawl");

        // Widening the window brings the older spend back, with its cost.
        let wide = db.spend_by_key(60).await.unwrap();
        assert_eq!(wide.len(), 2);
        assert_eq!(wide[0].token_name.as_deref(), Some("tok-old"));
        assert!(
            (wide[0].cost - 9.0).abs() < 1e-9,
            "cost DESC: old spender first"
        );
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

        let by_key = db.spend_by_key(7).await.unwrap();
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

        let by_service = db.spend_by_service(7).await.unwrap();
        assert_eq!(by_service.len(), 2);
        assert_eq!(by_service[0].service, "tavily");
        assert_eq!(by_service[0].requests, 2);
        assert!((by_service[0].cost - 5.0).abs() < 1e-9);
    }
}
