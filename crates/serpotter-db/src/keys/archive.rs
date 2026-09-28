use crate::{Db, DbError};

impl Db {
    /// Archive a proven-banned key, then hard-delete it — ONE transaction, so
    /// a crash can never leave a tombstone without the delete or a live row
    /// with a tombstone claiming it died.
    ///
    /// The tombstone is FINGERPRINT-ONLY by construction: the INSERT lists
    /// exactly the six archive columns and never touches `api_keys.key`, so no
    /// key material can reach the archive even if this SQL is later widened.
    ///
    /// `COALESCE(key_fingerprint, '')` is MANDATORY, not cosmetic:
    /// `key_fingerprint` is nullable since 0003, and a SELECT-supplied NULL
    /// would violate the archive column's `NOT NULL` and abort the whole ban
    /// transaction — a legacy row with no fingerprint would then be impossible
    /// to revoke.
    ///
    /// `reason` is a closed vocabulary (`'vendor_banned'`), never free text
    /// from an upstream error body.
    ///
    /// Returns whether a row was actually deleted, so callers keep the
    /// delete/no-delete distinction of a plain `DELETE`: an unknown id (double
    /// finish, multi-hold) archives nothing — `INSERT … SELECT` over a missing
    /// row matches zero rows — and reports `false`.
    pub async fn archive_and_delete_api_key(&self, id: i64) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        // Pre-clear the holder rows deliberately, as belt-and-braces: the
        // `ON DELETE CASCADE` on `api_key_leases.api_key_id` would remove
        // exactly these rows during the parent DELETE anyway. Naming them
        // explicitly is a readability choice — the cleanup is visible in the
        // statement instead of left to an implicit cascade — and it costs
        // nothing (same transaction, same set of rows).
        sqlx::query("DELETE FROM api_key_leases WHERE api_key_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO api_keys_archive \
               (api_key_id, service, key_fingerprint, reason, consecutive_fails, credits_remaining) \
             SELECT id, service, COALESCE(key_fingerprint, ''), 'vendor_banned', \
                    consecutive_fails, credits_remaining \
             FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let result = sqlx::query("DELETE FROM api_keys WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }
}
