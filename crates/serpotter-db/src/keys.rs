use crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES};
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyRow {
    pub id: i64,
    pub service: String,
    pub key: String,
    pub active: i64,
    pub consecutive_fails: i64,
}

/// Admin list/detail row with credits + inflight (not used on acquire paths).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyAdminRow {
    pub id: i64,
    pub service: String,
    pub key: String,
    pub active: i64,
    pub consecutive_fails: i64,
    pub credits_remaining: Option<i64>,
    pub credits_limit: Option<i64>,
    pub usage_synced_at: Option<String>,
    pub inflight: i64,
    /// Multi-hold reclaim deadline (UTC ISO from SQLite datetime).
    pub lease_until: Option<String>,
}

fn map_api_key_admin_row(r: &sqlx::sqlite::SqliteRow) -> Result<ApiKeyAdminRow, DbError> {
    Ok(ApiKeyAdminRow {
        id: r.try_get("id")?,
        service: r.try_get("service")?,
        key: r.try_get("key")?,
        active: r.try_get("active")?,
        consecutive_fails: r.try_get("consecutive_fails")?,
        credits_remaining: r.try_get("credits_remaining")?,
        credits_limit: r.try_get("credits_limit")?,
        usage_synced_at: r.try_get("usage_synced_at")?,
        inflight: r.try_get("inflight")?,
        lease_until: r.try_get("lease_until")?,
    })
}

impl Db {
    pub async fn insert_api_key(&self, service: &str, key: &str) -> Result<ApiKeyRow, DbError> {
        let result = sqlx::query(
            "INSERT INTO api_keys (service, key) VALUES (?, ?) \
             RETURNING id, service, key, active, consecutive_fails",
        )
        .bind(service)
        .bind(key)
        .fetch_one(&self.pool)
        .await?;

        Ok(ApiKeyRow {
            id: result.try_get("id")?,
            service: result.try_get("service")?,
            key: result.try_get("key")?,
            active: result.try_get("active")?,
            consecutive_fails: result.try_get("consecutive_fails")?,
        })
    }

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

    pub async fn set_api_key_credits(
        &self,
        id: i64,
        remaining: Option<i64>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET credits_remaining = ? WHERE id = ?")
            .bind(remaining)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Write credit snapshot from vendor usage sync. Resets consecutive_fails.
    pub async fn update_api_key_usage(
        &self,
        id: i64,
        remaining: i64,
        limit: i64,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                credits_remaining = ?, \
                credits_limit = ?, \
                usage_synced_at = datetime('now'), \
                consecutive_fails = 0 \
             WHERE id = ?",
        )
        .bind(remaining)
        .bind(limit)
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

    pub async fn list_api_keys(&self) -> Result<Vec<ApiKeyAdminRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, \
                    credits_remaining, credits_limit, usage_synced_at, inflight, lease_until \
             FROM api_keys ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(map_api_key_admin_row(&r)?);
        }
        Ok(out)
    }

    pub async fn get_api_key_admin(&self, id: i64) -> Result<Option<ApiKeyAdminRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, \
                    credits_remaining, credits_limit, usage_synced_at, inflight, lease_until \
             FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(map_api_key_admin_row(&r)?),
            None => None,
        })
    }

    pub async fn delete_api_key(&self, id: i64) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn set_api_key_active(&self, id: i64, active: bool) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE api_keys SET active = ?, consecutive_fails = CASE WHEN ? = 1 THEN 0 ELSE consecutive_fails END WHERE id = ?",
        )
        .bind(if active { 1i64 } else { 0i64 })
        .bind(if active { 1i64 } else { 0i64 })
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn count_api_keys(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM api_keys")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }

    pub async fn count_active_api_keys(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM api_keys WHERE active = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }

    /// Re-activate keys that have been inactive and idle for at least `hours`.
    /// Sets active=1 and consecutive_fails=0. Returns rows affected.
    pub async fn reenable_stale_keys(&self, hours: i64) -> Result<u64, DbError> {
        let hours = hours.max(0);
        let result = sqlx::query(
            "UPDATE api_keys SET active = 1, consecutive_fails = 0 \
             WHERE active = 0 \
               AND last_used_at IS NOT NULL \
               AND last_used_at < datetime('now', '-' || ? || ' hours')",
        )
        .bind(hours)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
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
