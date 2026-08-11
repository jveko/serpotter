use super::rows::{map_api_key_admin_row, ApiKeyAdminRow, ApiKeyRow};
use crate::{Db, DbError};
use sqlx::Row;

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

    /// Patch an api key. `service` / `key` are optional so a caller can rotate
    /// one field without re-sending the other; at least one must be `Some`.
    ///
    /// Rotating `key` resets `consecutive_fails` (a fresh secret is a clean
    /// slate — the old failures belonged to the leaked/retired key). Changing
    /// `service` drops the stored credit snapshot (`credits_*`, `usage_synced_at`)
    /// because those numbers belong to the old vendor account and must be
    /// re-synced before they can be trusted again.
    pub async fn update_api_key(
        &self,
        id: i64,
        service: Option<&str>,
        key: Option<&str>,
    ) -> Result<bool, DbError> {
        let result = sqlx::query(
            "UPDATE api_keys SET
                service = COALESCE(?, service),
                key = COALESCE(?, key),
                consecutive_fails = CASE WHEN ? IS NOT NULL THEN 0 ELSE consecutive_fails END,
                credits_remaining = CASE WHEN ? IS NOT NULL THEN NULL ELSE credits_remaining END,
                credits_limit = CASE WHEN ? IS NOT NULL THEN NULL ELSE credits_limit END,
                usage_synced_at = CASE WHEN ? IS NOT NULL THEN NULL ELSE usage_synced_at END
             WHERE id = ?",
        )
        .bind(service)
        .bind(key)
        .bind(key)
        .bind(service)
        .bind(service)
        .bind(service)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
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

    pub async fn list_api_keys(&self) -> Result<Vec<ApiKeyAdminRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, \
                    credits_remaining, credits_limit, usage_synced_at, inflight, lease_until, last_used_at \
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
                    credits_remaining, credits_limit, usage_synced_at, inflight, lease_until, last_used_at \
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
}
