use crate::{Db, DbError, LEASE_TTL_SECS, MAX_CONSECUTIVE_FAILURES};
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyRow {
    pub id: i64,
    pub service: String,
    pub key: String,
    pub active: i64,
    pub consecutive_fails: i64,
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

    /// Pick least-recently-used active key for service (credit priority then LRU).
    /// Skips keys with an unexpired soft lease; stamps `lease_until` on pick.
    pub async fn acquire_api_key(&self, service: &str) -> Result<Option<ApiKeyRow>, DbError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 \
               AND (lease_until IS NULL OR lease_until <= datetime('now')) \
             ORDER BY \
               CASE WHEN credits_remaining IS NULL OR credits_remaining > 0 THEN 1 ELSE 2 END, \
               last_used_at IS NOT NULL, \
               last_used_at ASC, \
               id ASC \
             LIMIT 1",
        )
        .bind(service)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };

        let id: i64 = r.try_get("id")?;
        sqlx::query(
            "UPDATE api_keys SET \
                last_used_at = datetime('now'), \
                lease_until = datetime('now', '+' || ? || ' seconds') \
             WHERE id = ?",
        )
        .bind(LEASE_TTL_SECS)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(Some(ApiKeyRow {
            id,
            service: r.try_get("service")?,
            key: r.try_get("key")?,
            active: r.try_get("active")?,
            consecutive_fails: r.try_get("consecutive_fails")?,
        }))
    }

    /// Acquire up to `n` distinct healthy keys (n clamped to 1..=10) in one transaction.
    /// Credit priority then LRU; zero-credit keys remain eligible as priority 2.
    /// Skips unexpired leases; stamps `lease_until` on each pick.
    pub async fn acquire_api_keys_batch(
        &self,
        service: &str,
        n: usize,
    ) -> Result<Vec<ApiKeyRow>, DbError> {
        let n = n.clamp(1, 10) as i64;
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 \
               AND (lease_until IS NULL OR lease_until <= datetime('now')) \
             ORDER BY \
               CASE WHEN credits_remaining IS NULL OR credits_remaining > 0 THEN 1 ELSE 2 END, \
               last_used_at IS NOT NULL, \
               last_used_at ASC, \
               id ASC \
             LIMIT ?",
        )
        .bind(service)
        .bind(n)
        .fetch_all(&mut *tx)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: i64 = r.try_get("id")?;
            sqlx::query(
                "UPDATE api_keys SET \
                    last_used_at = datetime('now'), \
                    lease_until = datetime('now', '+' || ? || ' seconds') \
                 WHERE id = ?",
            )
            .bind(LEASE_TTL_SECS)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            out.push(ApiKeyRow {
                id,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
            });
        }

        tx.commit().await?;
        Ok(out)
    }

    pub async fn report_api_key_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = 0, \
                last_used_at = datetime('now'), \
                lease_until = NULL \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn report_api_key_failure(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = consecutive_fails + 1, \
                last_used_at = datetime('now'), \
                lease_until = NULL, \
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
    /// Clears soft lease so the key is eligible again as priority-2.
    pub async fn report_api_key_exhausted(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                credits_remaining = 0, \
                last_used_at = datetime('now'), \
                lease_until = NULL \
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

    pub async fn list_api_keys(&self) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys ORDER BY id ASC",
        )
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
