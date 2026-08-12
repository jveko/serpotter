use super::rows::{map_api_key_admin_row, ApiKeyAdminRow, ApiKeyRow};
use crate::{Db, DbError};
use sha2::{Digest, Sha256};
use sqlx::Row;

/// sha256 hex of the plaintext key — the `api_keys.key_fingerprint` column
/// (migration 0003) exists so the pool can match a submitted secret to a row
/// without ever comparing plaintext keys in a query.
pub(crate) fn sha256_hex(plaintext: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(plaintext.as_bytes());
    format!("{:x}", hasher.finalize())
}

impl Db {
    /// True when `e` is a SQLite UNIQUE-constraint violation (duplicate
    /// `api_keys.key` on insert/rotation). Lets admin handlers map the
    /// constraint to a stable 409 instead of a raw 500 DatabaseError.
    pub fn is_unique_violation(e: &DbError) -> bool {
        matches!(e, DbError::Sqlx(sqlx::Error::Database(db)) if db.is_unique_violation())
    }

    pub async fn insert_api_key(&self, service: &str, key: &str) -> Result<ApiKeyRow, DbError> {
        let fingerprint = sha256_hex(key);
        let result = sqlx::query(
            "INSERT INTO api_keys (service, key, key_fingerprint) VALUES (?, ?, ?) \
             RETURNING id, service, key, active, consecutive_fails, key_fingerprint, \
                       budget_daily, budget_monthly",
        )
        .bind(service)
        .bind(key)
        .bind(fingerprint)
        .fetch_one(&self.pool)
        .await?;

        Ok(ApiKeyRow {
            id: result.try_get("id")?,
            service: result.try_get("service")?,
            key: result.try_get("key")?,
            active: result.try_get("active")?,
            consecutive_fails: result.try_get("consecutive_fails")?,
            key_fingerprint: result.try_get("key_fingerprint")?,
            budget_daily: result.try_get("budget_daily")?,
            budget_monthly: result.try_get("budget_monthly")?,
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
        // Rotating the key recomputes the fingerprint so the stored hash always
        // matches the live secret (a stale hash would silently lie about the key).
        let fingerprint = key.map(sha256_hex);
        let result = sqlx::query(
            "UPDATE api_keys SET
                service = COALESCE(?, service),
                key = COALESCE(?, key),
                key_fingerprint = CASE WHEN ? IS NOT NULL THEN ? ELSE key_fingerprint END,
                consecutive_fails = CASE WHEN ? IS NOT NULL THEN 0 ELSE consecutive_fails END,
                credits_remaining = CASE WHEN ? IS NOT NULL THEN NULL ELSE credits_remaining END,
                credits_limit = CASE WHEN ? IS NOT NULL THEN NULL ELSE credits_limit END,
                usage_synced_at = CASE WHEN ? IS NOT NULL THEN NULL ELSE usage_synced_at END
             WHERE id = ?",
        )
        .bind(service)
        .bind(key)
        .bind(key)
        .bind(fingerprint)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_writes_sha256_key_fingerprint() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let key = "tvly-fingerprint-test-0001";
        let row = db.insert_api_key("tavily", key).await.unwrap();
        assert_eq!(row.key_fingerprint, sha256_hex(key));
        assert!(!row.key_fingerprint.is_empty(), "fingerprint never empty");
    }

    #[tokio::test]
    async fn insert_fingerprint_is_hash_not_plaintext() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let key = "tvly-plaintext-never-stored-fp";
        let row = db.insert_api_key("tavily", key).await.unwrap();
        // Deterministic sha256 hex: stable, 64 chars, never the plaintext.
        assert_eq!(row.key_fingerprint, sha256_hex(key));
        assert_eq!(sha256_hex(key), sha256_hex(key), "same input → same hash");
        assert_ne!(
            row.key_fingerprint, key,
            "column must not hold the plaintext"
        );
        assert_eq!(row.key_fingerprint.len(), 64, "sha256 hex is 64 chars");
        assert!(
            row.key_fingerprint.chars().all(|c| c.is_ascii_hexdigit()),
            "fingerprint is lowercase hex"
        );
    }

    #[tokio::test]
    async fn rotate_recomputes_key_fingerprint() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let row = db
            .insert_api_key("exa", "exa-old-fingerprint-key")
            .await
            .unwrap();
        assert_eq!(row.key_fingerprint, sha256_hex("exa-old-fingerprint-key"));

        let ok = db
            .update_api_key(row.id, None, Some("exa-new-fingerprint-key"))
            .await
            .unwrap();
        assert!(ok);
        let after = db.get_api_key(row.id).await.unwrap().unwrap();
        assert_eq!(after.key, "exa-new-fingerprint-key");
        assert_eq!(
            after.key_fingerprint,
            sha256_hex("exa-new-fingerprint-key"),
            "rotation must refresh the stored hash"
        );
    }

    #[tokio::test]
    async fn duplicate_insert_is_unique_violation() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        db.insert_api_key("tavily", "tvly-duplicate-409")
            .await
            .unwrap();
        let err = db
            .insert_api_key("tavily", "tvly-duplicate-409")
            .await
            .expect_err("second insert of the same key must fail");
        assert!(
            Db::is_unique_violation(&err),
            "UNIQUE violation must be detectable: {err}"
        );
    }

    #[tokio::test]
    async fn rotate_to_duplicate_key_is_unique_violation() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let a = db
            .insert_api_key("tavily", "tvly-rotate-target-001")
            .await
            .unwrap();
        let b = db
            .insert_api_key("tavily", "tvly-rotate-source-002")
            .await
            .unwrap();
        let err = db
            .update_api_key(b.id, None, Some("tvly-rotate-target-001"))
            .await
            .expect_err("rotating onto an existing key must fail");
        assert!(
            Db::is_unique_violation(&err),
            "UNIQUE violation must be detectable: {err}"
        );
        // a untouched (the transaction/statement is a single UPDATE, so no partial write)
        let a_after = db.get_api_key(a.id).await.unwrap().unwrap();
        assert_eq!(a_after.key, "tvly-rotate-target-001");
    }

    #[tokio::test]
    async fn legacy_null_fingerprint_rows_still_decode_on_read_paths() {
        // Pre-wave rows (migration 0003, nullable key_fingerprint) have NULL in
        // the column. The read paths must COALESCE it, not error, or every
        // existing key on an upgraded server breaks acquire/search/extract.
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        sqlx::query(
            "INSERT INTO api_keys (service, key, key_fingerprint) VALUES ('tavily', 'tvly-legacy-null', NULL)",
        )
        .execute(db.pool())
        .await
        .unwrap();

        let by_id = db.get_api_key(1).await.unwrap().expect("row exists");
        assert_eq!(by_id.key, "tvly-legacy-null");
        assert_eq!(by_id.key_fingerprint, "", "NULL coalesced to empty");

        let listed = db.list_active_keys_for_service("tavily").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key_fingerprint, "");

        let acquired = db
            .acquire_api_key_shared("tavily", 3, 90, 100)
            .await
            .unwrap()
            .expect("legacy NULL row must remain acquirable");
        assert_eq!(acquired.key, "tvly-legacy-null");
        assert_eq!(acquired.key_fingerprint, "");
    }
}
