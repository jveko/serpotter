use super::rows::ApiKeyRow;
use crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES};
use sqlx::{Executor, Row};

/// Reclaim UPDATE for `api_keys` — the single source of truth shared by the
/// public helper and the acquire-path transaction (see [`Db::reclaim_expired_holds`]).
const RECLAIM_API_KEYS_SQL: &str = "UPDATE api_keys SET inflight = 0, lease_until = NULL \
     WHERE lease_until IS NOT NULL AND lease_until <= datetime('now')";

impl Db {
    /// Zero inflight and clear lease when hold deadline has passed.
    pub async fn reclaim_expired_key_holds(&self) -> Result<u64, DbError> {
        Self::reclaim_expired_holds(&self.pool, RECLAIM_API_KEYS_SQL).await
    }

    /// Executor-generic runner for the shared reclaim UPDATE (keys + nodes).
    /// Works on both `&self.pool` (public helpers) and a transaction handle
    /// (inside the acquire paths), so the SQL cannot drift between the two.
    pub(crate) async fn reclaim_expired_holds<'e, E>(
        executor: E,
        sql: &'static str,
    ) -> Result<u64, DbError>
    where
        E: Executor<'e, Database = sqlx::Sqlite>,
    {
        let r = sqlx::query(sql).execute(executor).await?;
        Ok(r.rows_affected())
    }

    /// Process-start hygiene: drop all key holds.
    pub async fn zero_all_key_inflight(&self) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET inflight = 0, lease_until = NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Shared-cap acquire: reclaim expired holds, pick Envoy-damped credit score under max, optimistic bump.
    ///
    /// Score (non-exhausted): `(effective_C * KEY_CREDIT_SCORE_SCALE) / (inflight + 1)` DESC.
    /// `effective_C` = `credits_remaining` if non-NULL, else `unknown_credit_weight` (clamped ≥ 1).
    /// Exhausted (`credits_remaining = 0`) is last tier but still eligible.
    ///
    /// B23 budget gate: a key with `budget_daily`/`budget_monthly` set is
    /// excluded from THIS pick when the SERVICE window spend (`usage_daily`
    /// cost for the key's service, today / since month start) already meets or
    /// exceeds the budget — documented as a service-window budget: all keys of
    /// one service share the vendor spend window. Keys without budgets are
    /// never gated. When every candidate is budget-exhausted the acquire
    /// returns `None` (the keypool then fails the request as no-healthy-key /
    /// acquire-timeout — the budget signal stays in the db layer, per design).
    pub async fn acquire_api_key_shared(
        &self,
        service: &str,
        max_inflight: i64,
        hold_ttl_secs: i64,
        unknown_credit_weight: i64,
    ) -> Result<Option<ApiKeyRow>, DbError> {
        let unknown_credit_weight = unknown_credit_weight.max(1);
        let hold_ttl_secs = hold_ttl_secs.max(1);
        let mut tx = self.pool.begin().await?;
        Self::reclaim_expired_holds(&mut *tx, RECLAIM_API_KEYS_SQL).await?;

        let row = sqlx::query(
            "SELECT ak.id, ak.service, ak.key, ak.active, ak.consecutive_fails, \
                    COALESCE(ak.key_fingerprint, '') AS key_fingerprint, \
                    ak.budget_daily, ak.budget_monthly \
             FROM api_keys ak \
             WHERE ak.service = ? AND ak.active = 1 AND ak.inflight < ? \
               AND NOT ( \
                 (ak.budget_daily IS NOT NULL AND (SELECT COALESCE(SUM(cost), 0) FROM usage_daily \
                     WHERE service = ak.service AND date = date('now')) >= ak.budget_daily) \
                 OR \
                 (ak.budget_monthly IS NOT NULL AND (SELECT COALESCE(SUM(cost), 0) FROM usage_daily \
                     WHERE service = ak.service AND date >= strftime('%Y-%m-01', 'now')) >= ak.budget_monthly) \
               ) \
             ORDER BY \
               CASE WHEN ak.credits_remaining = 0 THEN 1 ELSE 0 END, \
               (CASE \
                  WHEN ak.credits_remaining IS NULL THEN ? \
                  ELSE ak.credits_remaining \
                END * ?) / (ak.inflight + 1) DESC, \
               ak.last_used_at IS NOT NULL, ak.last_used_at ASC, ak.id ASC \
             LIMIT 1",
        )
        .bind(service)
        .bind(max_inflight)
        .bind(unknown_credit_weight)
        .bind(crate::KEY_CREDIT_SCORE_SCALE)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: i64 = r.try_get("id")?;
        let updated = sqlx::query(
            "UPDATE api_keys SET \
                inflight = inflight + 1, \
                last_used_at = datetime('now'), \
                lease_until = datetime('now', '+' || ? || ' seconds') \
             WHERE id = ? AND active = 1 AND inflight < ?",
        )
        .bind(hold_ttl_secs)
        .bind(id)
        .bind(max_inflight)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }
        tx.commit().await?;
        Ok(Some(ApiKeyRow {
            id,
            service: r.try_get("service")?,
            key: r.try_get("key")?,
            active: r.try_get("active")?,
            consecutive_fails: r.try_get("consecutive_fails")?,
            key_fingerprint: r.try_get("key_fingerprint")?,
            budget_daily: r.try_get("budget_daily")?,
            budget_monthly: r.try_get("budget_monthly")?,
        }))
    }

    /// Multi-hold-safe release: decrement inflight; clear lease_until only when now 0.
    pub async fn release_api_key_inflight(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Active keys for a service (empty-inventory fail-fast).
    pub async fn count_active_keys(&self, service: &str) -> Result<i64, DbError> {
        let row =
            sqlx::query("SELECT COUNT(*) AS c FROM api_keys WHERE service = ? AND active = 1")
                .bind(service)
                .fetch_one(&self.pool)
                .await?;
        Ok(row.try_get("c")?)
    }

    /// Success report + multi-hold-safe inflight decrement.
    pub async fn report_api_key_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = 0, \
                last_used_at = datetime('now'), \
                credits_remaining = CASE \
                  WHEN credits_remaining IS NULL THEN NULL \
                  WHEN credits_remaining <= 0 THEN 0 \
                  ELSE credits_remaining - 1 \
                END, \
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Failure report + multi-hold-safe inflight decrement; disable at max fails.
    pub async fn report_api_key_failure(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = consecutive_fails + 1, \
                last_used_at = datetime('now'), \
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END, \
                active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END \
             WHERE id = ?",
        )
        .bind(MAX_CONSECUTIVE_FAILURES)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Zero tracked credits (mysearch parity). `NULL` credits (providers without
    /// a usage API — Exa/xAI) stay `NULL` so those keys are not permanently
    /// demoted to the exhausted-last tier by a single 429. Does NOT set
    /// active=0; hard-disable is fail@3 (auth-class) only.
    /// Multi-hold-safe inflight decrement; clears lease when last hold ends.
    pub async fn report_api_key_exhausted(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                credits_remaining = CASE \
                  WHEN credits_remaining IS NULL THEN NULL \
                  ELSE 0 \
                END, \
                last_used_at = datetime('now'), \
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Test helper: force `lease_until` (ISO-ish SQLite datetime text, or NULL).
    pub async fn set_api_key_lease_until(
        &self,
        id: i64,
        lease_until: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET lease_until = ? WHERE id = ?")
            .bind(lease_until)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Active keys for a service, never-synced first then oldest sync.
    pub async fn list_active_keys_for_service(
        &self,
        service: &str,
    ) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint, \
                    budget_daily, budget_monthly FROM api_keys \
             WHERE service = ? AND active = 1 \
             ORDER BY usage_synced_at IS NOT NULL, usage_synced_at ASC, id ASC",
        )
        .bind(service)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
                key_fingerprint: r.try_get("key_fingerprint")?,
                budget_daily: r.try_get("budget_daily")?,
                budget_monthly: r.try_get("budget_monthly")?,
            });
        }
        Ok(out)
    }

    pub async fn get_api_key(&self, id: i64) -> Result<Option<ApiKeyRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint, \
                    budget_daily, budget_monthly FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
                key_fingerprint: r.try_get("key_fingerprint")?,
                budget_daily: r.try_get("budget_daily")?,
                budget_monthly: r.try_get("budget_monthly")?,
            }),
            None => None,
        })
    }

    /// Test helper: force `last_used_at` (SQLite datetime text).
    pub async fn set_api_key_last_used_at(
        &self,
        id: i64,
        last_used_at: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
            .bind(last_used_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
