use super::rows::ApiKeyRow;
use crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES};
use sqlx::Row;

#[derive(Clone, Debug)]
pub struct KeyLease {
    pub key: ApiKeyRow,
    pub token: i64,
}

impl std::ops::Deref for KeyLease {
    type Target = ApiKeyRow;
    fn deref(&self) -> &Self::Target {
        &self.key
    }
}

const SQL_RECLAIM_KEY_LEASES: &str =
    "DELETE FROM api_key_leases WHERE lease_until <= datetime('now')";
const SQL_RECONCILE_KEYS: &str = "UPDATE api_keys SET \
    inflight = (SELECT COUNT(*) FROM api_key_leases WHERE api_key_id = api_keys.id), \
    lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = api_keys.id) \
    WHERE inflight != (SELECT COUNT(*) FROM api_key_leases WHERE api_key_id = api_keys.id) \
       OR lease_until IS NOT (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = api_keys.id)";

impl Db {
    async fn reclaim_key_leases<'e, E>(executor: E) -> Result<u64, DbError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        Ok(sqlx::query(SQL_RECLAIM_KEY_LEASES)
            .execute(executor)
            .await?
            .rows_affected())
    }

    pub async fn reclaim_expired_key_holds(&self) -> Result<u64, DbError> {
        let mut tx = self.pool.begin().await?;
        let removed = Self::reclaim_key_leases(&mut *tx).await?;
        sqlx::query(SQL_RECONCILE_KEYS).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(removed)
    }

    pub async fn zero_all_key_inflight(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM api_key_leases")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE api_keys SET inflight = 0, lease_until = NULL")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn acquire_api_key_shared(
        &self,
        service: &str,
        max_inflight: i64,
        hold_ttl_secs: i64,
        unknown_credit_weight: i64,
    ) -> Result<Option<KeyLease>, DbError> {
        let unknown_credit_weight = unknown_credit_weight.max(1);
        let hold_ttl_secs = hold_ttl_secs.max(1);
        let mut tx = self.pool.begin().await?;
        Self::reclaim_key_leases(&mut *tx).await?;
        sqlx::query(SQL_RECONCILE_KEYS).execute(&mut *tx).await?;
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint FROM api_keys \
             WHERE service = ? AND active = 1 AND inflight < ? \
             ORDER BY CASE WHEN credits_remaining = 0 THEN 1 ELSE 0 END, \
               (CASE WHEN credits_remaining IS NULL THEN ? ELSE credits_remaining END * ?) / (inflight + 1) DESC, \
               last_used_at IS NOT NULL, last_used_at ASC, id ASC LIMIT 1",
        ).bind(service).bind(max_inflight).bind(unknown_credit_weight)
         .bind(crate::KEY_CREDIT_SCORE_SCALE).fetch_optional(&mut *tx).await?;
        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: i64 = r.try_get("id")?;
        let updated = sqlx::query(
            "UPDATE api_keys SET inflight = inflight + 1, last_used_at = datetime('now') \
             WHERE id = ? AND active = 1 AND inflight < ?",
        )
        .bind(id)
        .bind(max_inflight)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }
        let token: i64 = sqlx::query_scalar(
            "INSERT INTO api_key_leases(api_key_id, lease_until) VALUES (?, datetime('now', '+' || ? || ' seconds')) RETURNING token",
        ).bind(id).bind(hold_ttl_secs).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE api_keys SET lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = ?) WHERE id = ?")
            .bind(id).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(KeyLease {
            token,
            key: ApiKeyRow {
                id,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
                key_fingerprint: r.try_get("key_fingerprint")?,
            },
        }))
    }

    async fn release_key_token(
        &self,
        token: i64,
        health: Option<&str>,
        max_fails: i64,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let key_id: Option<i64> =
            sqlx::query_scalar("DELETE FROM api_key_leases WHERE token = ? RETURNING api_key_id")
                .bind(token)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(id) = key_id else {
            tx.commit().await?;
            return Ok(false);
        };
        match health {
            Some("success") => {
                sqlx::query("UPDATE api_keys SET consecutive_fails = 0, last_used_at = datetime('now'), credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL WHEN credits_remaining <= 0 THEN 0 ELSE credits_remaining - 1 END WHERE id = ?").bind(id).execute(&mut *tx).await?;
            }
            Some("failure") => {
                sqlx::query("UPDATE api_keys SET consecutive_fails = consecutive_fails + 1, last_used_at = datetime('now'), active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END, disabled_reason = CASE WHEN disabled_reason IS NULL AND consecutive_fails + 1 >= ? THEN 'auth_fail' ELSE disabled_reason END WHERE id = ?").bind(max_fails).bind(max_fails).bind(id).execute(&mut *tx).await?;
            }
            Some("exhausted") => {
                sqlx::query("UPDATE api_keys SET credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL ELSE 0 END, last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&mut *tx).await?;
            }
            Some("payment_required") => {
                sqlx::query("UPDATE api_keys SET credits_remaining = 0, last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&mut *tx).await?;
            }
            Some("suspended") => {
                sqlx::query("UPDATE api_keys SET active = 0, disabled_reason = 'vendor_suspended', last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&mut *tx).await?;
            }
            _ => {}
        }
        sqlx::query("UPDATE api_keys SET inflight = (SELECT COUNT(*) FROM api_key_leases WHERE api_key_id = ?), lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = ?) WHERE id = ?")
            .bind(id).bind(id).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn count_active_keys(&self, service: &str) -> Result<i64, DbError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE service = ? AND active = 1")
                .bind(service)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    pub async fn release_api_key_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_key_token(token, None, MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn report_api_key_success_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_key_token(token, Some("success"), MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn report_api_key_failure_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_key_token(token, Some("failure"), MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn report_api_key_exhausted_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_key_token(token, Some("exhausted"), MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn report_api_key_payment_required_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_key_token(token, Some("payment_required"), MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn suspend_api_key_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_key_token(token, Some("suspended"), MAX_CONSECUTIVE_FAILURES)
            .await
    }

    pub async fn refresh_api_key_lease(
        &self,
        token: i64,
        hold_ttl_secs: i64,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '+' || ? || ' seconds') WHERE token = ?")
            .bind(hold_ttl_secs.max(1)).bind(token).execute(&mut *tx).await?.rows_affected() > 0;
        if changed {
            sqlx::query("UPDATE api_keys SET lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = api_keys.id) WHERE id = (SELECT api_key_id FROM api_key_leases WHERE token = ?)").bind(token).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    pub async fn set_api_key_lease_until(
        &self,
        id: i64,
        lease_until: Option<&str>,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        match lease_until {
            Some(v) => {
                sqlx::query("UPDATE api_key_leases SET lease_until = ? WHERE api_key_id = ?")
                    .bind(v)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM api_key_leases WHERE api_key_id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE api_keys SET lease_until = ? WHERE id = ?")
            .bind(lease_until)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_active_keys_for_service(
        &self,
        service: &str,
    ) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query("SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint FROM api_keys WHERE service = ? AND active = 1 ORDER BY usage_synced_at IS NOT NULL, usage_synced_at ASC, id ASC")
            .bind(service).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|r| {
                Ok(ApiKeyRow {
                    id: r.try_get("id")?,
                    service: r.try_get("service")?,
                    key: r.try_get("key")?,
                    active: r.try_get("active")?,
                    consecutive_fails: r.try_get("consecutive_fails")?,
                    key_fingerprint: r.try_get("key_fingerprint")?,
                })
            })
            .collect()
    }
    pub async fn get_api_key(&self, id: i64) -> Result<Option<ApiKeyRow>, DbError> {
        let r = sqlx::query("SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint FROM api_keys WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        r.map(|r| {
            Ok(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
                key_fingerprint: r.try_get("key_fingerprint")?,
            })
        })
        .transpose()
    }
    pub async fn set_api_key_last_used_at(
        &self,
        id: i64,
        value: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
            .bind(value)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_success_lease`.
    pub async fn note_key_health_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET consecutive_fails = 0, last_used_at = datetime('now'), credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL WHEN credits_remaining <= 0 THEN 0 ELSE credits_remaining - 1 END WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_failure_lease`.
    pub async fn note_key_health_failure(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET consecutive_fails = consecutive_fails + 1, last_used_at = datetime('now'), active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END, disabled_reason = CASE WHEN disabled_reason IS NULL AND consecutive_fails + 1 >= ? THEN 'auth_fail' ELSE disabled_reason END WHERE id = ?").bind(MAX_CONSECUTIVE_FAILURES).bind(MAX_CONSECUTIVE_FAILURES).bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_exhausted_lease`.
    pub async fn note_key_health_exhausted(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL ELSE 0 END, last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_payment_required_lease`.
    pub async fn note_key_health_payment_required(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET credits_remaining = 0, last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only suspension note; never releases a lease.
    pub async fn note_key_health_suspended(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET active = 0, disabled_reason = 'vendor_suspended', last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
}
