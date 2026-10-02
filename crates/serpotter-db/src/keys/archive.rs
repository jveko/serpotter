use crate::{Db, DbError};

/// Closed, enum-bounded vocabulary for `api_keys_archive.reason`.
///
/// The archive reason is an allowlist compiled into this type: callers pick a
/// variant, never supply a string, so no upstream error body or free text can
/// ever reach the `reason` column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyArchiveReason {
    /// A proven vendor ban (the live ban path).
    VendorBanned,
    /// The daily health probe received a definitive 401 from the vendor.
    ProbeAuth401,
}

impl ApiKeyArchiveReason {
    /// The exact string stored in `api_keys_archive.reason`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            ApiKeyArchiveReason::VendorBanned => "vendor_banned",
            ApiKeyArchiveReason::ProbeAuth401 => "probe_auth_401",
        }
    }
}

impl Db {
    /// Archive a proven-dead key, then hard-delete it — ONE transaction, so
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
    /// `reason` is a vocabulary closed BY THE ENUM ([`ApiKeyArchiveReason`]:
    /// `vendor_banned` | `probe_auth_401`), never free text from an upstream
    /// error body.
    ///
    /// Returns whether a row was actually deleted, so callers keep the
    /// delete/no-delete distinction of a plain `DELETE`: an unknown id (double
    /// finish, multi-hold) archives nothing — `INSERT … SELECT` over a missing
    /// row matches zero rows — and reports `false`.
    pub async fn archive_and_delete_api_key(
        &self,
        id: i64,
        reason: ApiKeyArchiveReason,
    ) -> Result<bool, DbError> {
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
             SELECT id, service, COALESCE(key_fingerprint, ''), ?, \
                    consecutive_fails, credits_remaining \
             FROM api_keys WHERE id = ?",
        )
        .bind(reason.as_db_str())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_stores_probe_auth_401_reason() {
        let db = Db::connect_for_test().await;
        let row = db
            .insert_api_key("tavily", "tvly-probe-401")
            .await
            .unwrap();
        let deleted = db
            .archive_and_delete_api_key(row.id, ApiKeyArchiveReason::ProbeAuth401)
            .await
            .expect("archive+delete");
        assert!(deleted, "a live row must be reported as deleted");
        let reason: String =
            sqlx::query_scalar("SELECT reason FROM api_keys_archive WHERE api_key_id = ?")
                .bind(row.id)
                .fetch_one(db.pool())
                .await
                .expect("archive tombstone");
        assert_eq!(
            reason, "probe_auth_401",
            "the probe path must record its own allowlisted reason"
        );
        assert!(
            db.get_api_key(row.id).await.unwrap().is_none(),
            "the live key row must be gone after the probe archive"
        );
    }
}
