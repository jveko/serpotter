use super::rows::ApiKeyRow;
use crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES};
use sqlx::Row;

impl Db {
    /// Zero inflight and clear lease when hold deadline has passed.
    pub async fn reclaim_expired_key_holds(&self) -> Result<u64, DbError> {
        let r = sqlx::query(
            "UPDATE api_keys SET inflight = 0, lease_until = NULL \
             WHERE lease_until IS NOT NULL AND lease_until <= datetime('now')",
        )
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    /// Process-start hygiene: drop all key holds.
    pub async fn zero_all_key_inflight(&self) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET inflight = 0, lease_until = NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Shared-cap acquire: reclaim expired holds, pick least inflight under max, optimistic bump.
    pub async fn acquire_api_key_shared(
        &self,
        service: &str,
        max_inflight: i64,
        hold_ttl_secs: i64,
    ) -> Result<Option<ApiKeyRow>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE api_keys SET inflight = 0, lease_until = NULL \
             WHERE lease_until IS NOT NULL AND lease_until <= datetime('now')",
        )
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 AND inflight < ? \
             ORDER BY \
               CASE WHEN credits_remaining IS NULL OR credits_remaining > 0 THEN 1 ELSE 2 END, \
               inflight ASC, \
               last_used_at IS NOT NULL, last_used_at ASC, id ASC \
             LIMIT 1",
        )
        .bind(service)
        .bind(max_inflight)
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
        let row = sqlx::query(
            "SELECT COUNT(*) AS c FROM api_keys WHERE service = ? AND active = 1",
        )
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

    /// Zero credits (mysearch parity). Does NOT set active=0; hard-disable is fail@3 only.
    /// Multi-hold-safe inflight decrement; clears lease when last hold ends.
    pub async fn report_api_key_exhausted(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                credits_remaining = 0, \
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
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
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
            });
        }
        Ok(out)
    }

    pub async fn get_api_key(&self, id: i64) -> Result<Option<ApiKeyRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys WHERE id = ?",
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
