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
            "INSERT INTO api_keys (service, key, key_fingerprint) \
             VALUES (?, ?, ?) \
             RETURNING id, service, key, active, consecutive_fails, key_fingerprint",
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
    /// slate — the old failures belonged to the leaked/retired key), and either
    /// an identity change (`key` OR `service`) clears `disabled_reason`: a
    /// `'vendor_suspended'` marker describes the OLD vendor account, so leaving
    /// it on a row that now points at a different vendor would strand a working
    /// key outside the re-enable cron forever. Changing `service` also drops
    /// the stored credit snapshot (`credits_*`, `usage_synced_at`) because those
    /// numbers belong to the old vendor account and must be re-synced before
    /// they can be trusted again.
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
                usage_synced_at = CASE WHEN ? IS NOT NULL THEN NULL ELSE usage_synced_at END,
                disabled_reason = CASE WHEN ? IS NOT NULL OR ? IS NOT NULL THEN NULL ELSE disabled_reason END
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
        .bind(key)
        .bind(service)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Write credit snapshot from vendor usage sync.
    ///
    /// Deliberately does NOT touch `consecutive_fails` (or the inflight/lease
    /// columns): a credential-usage write is not a health signal, so a
    /// successful sync must not erase fail@3 history and hand a still-broken
    /// key a clean bill of health. Soft-burn of `credits_remaining` lives in
    /// the keypool (`report_api_key_success`) and re-enable clears the counters.
    /// Lease ownership stays with the keypool — see the holder-set schema.
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
                usage_synced_at = datetime('now') \
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
                    credits_remaining, credits_limit, usage_synced_at, inflight, lease_until, \
                    last_used_at, disabled_reason \
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
                    credits_remaining, credits_limit, usage_synced_at, inflight, lease_until, \
                    last_used_at, disabled_reason \
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
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM api_key_leases WHERE api_key_id = ?")
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

    /// Operator/admin active toggle. Enabling clears `disabled_reason` (an
    /// admin override is the only way a vendor-suspended row returns to
    /// rotation — see [`Db::reenable_stale_keys`]). Disabling records
    /// `'manual'` only when no reason is recorded yet, so it can never
    /// overwrite a `vendor_suspended` marker and hand that key back to the
    /// cron.
    pub async fn set_api_key_active(&self, id: i64, active: bool) -> Result<bool, DbError> {
        let on = if active { 1i64 } else { 0i64 };
        let result = sqlx::query(
            "UPDATE api_keys SET active = ?, \
                consecutive_fails = CASE WHEN ? = 1 THEN 0 ELSE consecutive_fails END, \
                disabled_reason = CASE \
                  WHEN ? = 1 THEN NULL \
                  WHEN disabled_reason IS NULL THEN 'manual' \
                  ELSE disabled_reason \
                END \
             WHERE id = ?",
        )
        .bind(on)
        .bind(on)
        .bind(on)
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

    /// Floor for the re-enable window. Below 1 hour the predicate degenerates:
    /// `0` makes `last_used_at < datetime('now')` true for EVERY idle inactive
    /// row (fail@3 backoff is effectively off — the next 15-minute tick hands
    /// the dead key straight back to rotation), and a negative value would form
    /// `datetime('now', '--1 hours')`, which SQLite evaluates to NULL and which
    /// therefore matches nothing — a silent no-op that reads as "nothing to
    /// re-enable". Clamped here so the SQL modifier can never take a `--N`
    /// form regardless of the caller; `cron.rs` warns loudly at startup and
    /// `docs/ops/env.md` documents the range.
    pub const REENABLE_MIN_HOURS: i64 = 1;

    /// Re-activate keys that have been inactive and idle for at least
    /// `hours` (clamped to [`Self::REENABLE_MIN_HOURS`]), EXCEPT rows the
    /// vendor itself deactivated.
    ///
    /// `disabled_reason = 'vendor_suspended'` is written by
    /// [`Db::suspend_api_key`] when a vendor answers a permanent ban with a
    /// *suspend* disposition (Tavily/Exa/xAI: `401 "account … has been
    /// deactivated"`). Before schema 18 such a row was cron-eligible BY
    /// CONSTRUCTION — `active = 0` plus idle past the window, with no column
    /// distinguishing a vendor deactivation from a transient fail@3 disable — so
    /// the intended self-heal would have brought it back, the next acquire would
    /// have attempted it, and the vendor would have answered `401` again.
    ///
    /// **Status: latent defect, not an observed cost.** Two things are
    /// established. First, the eligibility: pre-18 there was no way to tell a
    /// vendor deactivation from a transient disable, so a suspended row met the
    /// cron's own predicate (`active = 0` + idle past the window) by
    /// construction. Second, the cron's observed effect: exactly ONE non-empty
    /// pass in 14 days of prod logs (2026-08-27T07:31:50Z, `n=24`).
    ///
    /// **Whether revival ever fed re-attempts is UNRESOLVED, and the logs cannot
    /// settle it.** Zero repeat bans were measured (3,098 events across 3,098
    /// DISTINCT `key_id`s) — but both hypotheses predict exactly that. Revival may
    /// never have run, or it may have run while the revived rows sat unattempted:
    /// `acquire_api_key_shared` orders by credit/plan score, then
    /// `last_used_at IS NOT NULL` (so NULL — a key never attempted, which is every
    /// row `insert_api_key` creates — sorts FIRST), then oldest-used. A revived row
    /// therefore heads the *already-touched* set, not the pick order, and the pool
    /// plainly had untouched tail: 3,098 bans over 12 days with a monotonic
    /// `key_id` frontier is the signature of first contact. The Aug-27 cohort was
    /// eligible by Aug 28 and produced no repeats, which is consistent with either
    /// story. No date on which repeats would begin is claimed or defensible.
    ///
    /// What CAN be corroborated about the missing rows: 307 suspension events leave
    /// only 128 inactive tavily rows, and rows leave the inactive set by deletion
    /// and admin edits too. That is not proof of revival, and the reverse
    /// inference — subtracting an instantaneous row count from a cumulative event
    /// count — is not arithmetic in either direction. Corroborating the caution,
    /// the one `n=24` pass proves only that 24 idle inactive rows came back: on
    /// the pre-18 schema its composition is unknowable, necessarily mixing vendor
    /// suspensions with ordinary fail@3 auth disables (firecrawl rows are
    /// hard-deleted, so they cannot appear) — the very indistinguishability
    /// `disabled_reason` removes. That is the honest case for this migration: it
    /// deletes an eligibility rather than stopping a cost the deployment is
    /// currently paying. Rank it below the request classification work when
    /// deciding what to deploy for.
    ///
    /// One pattern to rule out before believing any revival-loop story at all:
    /// tavily suspensions arrived in two bursts with ZERO `key_id` overlap — 192
    /// keys spread over `10106-10363` (258 ids, ~74% dense) on Aug 27
    /// 03:40-10:02Z, nothing for the 11 days between, then 106 over
    /// `10603-10836` (234 ids, ~45% dense) inside eight minutes on Sep 8
    /// 22:03-22:11Z, plus 9 more on Sep 9. A dense, roughly-ordered id band dying
    /// all at once reads as an imported batch of already-dead accounts being
    /// traversed for the first time, not as accounts re-killed after revival.
    ///
    /// That traversal shape is what the pick order predicts rather than merely
    /// resembles: every never-attempted row shares `last_used_at IS NULL` and the
    /// same unknown-credit weight, so the tiebreak `ak.id ASC` drains a new
    /// import in broadly ascending id order. Measured, not assumed — 159 of 191
    /// consecutive Aug-27 bans, 104 of 105 Sep-8, and all 8 Sep-9 steps move
    /// UPWARD (that last cohort fully monotonic), with local inversions like
    /// `10623 → 10621` and `10112 → 10110`. Those local out-of-order steps can
    /// only come from OVERLAPPING attempts: inside one leg the loop is strictly
    /// sequential (`for attempt in 1..=MAX_ATTEMPTS`, with the ban WARN emitted
    /// after the response is handled), so pick → request → respond → log → next
    /// pick cannot invert itself however uneven the latencies are. Once attempts
    /// do overlap — hybrid/blend legs via `tokio::join!`, separate simultaneous
    /// requests, or a key re-picked after release under `ak.inflight < ?` — the
    /// logged order tracks COMPLETION while the `key_id` was stamped at
    /// SELECTION, so logged order drifts from selection order. Which of those
    /// produced any given inversion these logs cannot say, and naming one would
    /// be over-claiming.
    /// Either way this stays corroboration rather than proof, and it does not
    /// close the recurrence question above.
    ///
    /// An operator can still force such a row back through
    /// [`Db::set_api_key_active`], which clears the reason; `NULL` (legacy rows
    /// never disabled) and `'manual'` keep the self-healing behavior they had
    /// before schema 18, because for those the revive *is* the recovery path.
    pub async fn reenable_stale_keys(&self, hours: i64) -> Result<u64, DbError> {
        let hours = hours.max(Self::REENABLE_MIN_HOURS);
        let result = sqlx::query(
            "UPDATE api_keys SET active = 1, consecutive_fails = 0, \
                    disabled_reason = NULL \
             WHERE active = 0 \
               AND disabled_reason IS NOT 'vendor_suspended' \
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
        assert_eq!(acquired.key.key, "tvly-legacy-null");
        assert_eq!(acquired.key.key_fingerprint, "");
    }

    /// A credit sync is a billing read, not a health signal: it must not wipe
    /// fail@3 history, or a broken key gets a clean bill of health every 15
    /// minutes and is re-enabled the instant the window expires.
    #[tokio::test]
    async fn update_api_key_usage_preserves_consecutive_fails() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let row = db
            .insert_api_key("tavily", "tvly-usage-sync")
            .await
            .unwrap();
        sqlx::query("UPDATE api_keys SET consecutive_fails = 2 WHERE id = ?")
            .bind(row.id)
            .execute(db.pool())
            .await
            .unwrap();

        db.update_api_key_usage(row.id, 900, 1000).await.unwrap();

        let after = db.get_api_key_admin(row.id).await.unwrap().unwrap();
        assert_eq!(after.consecutive_fails, 2, "sync is not a health signal");
        assert_eq!(after.credits_remaining, Some(900));
        assert_eq!(after.credits_limit, Some(1000));
        assert!(after.usage_synced_at.is_some(), "sync stamp still written");
    }

    /// `KEY_REENABLE_AFTER_HOURS=0` would otherwise make every idle inactive
    /// row eligible on the next tick (fail@3 backoff off). Clamped to 1h, a
    /// 30-minute-old disable is NOT revived.
    #[tokio::test]
    async fn reenable_stale_keys_clamps_zero_to_the_one_hour_floor() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let row = db
            .insert_api_key("tavily", "tvly-floor-zero")
            .await
            .unwrap();
        db.set_api_key_active(row.id, false).await.unwrap();
        sqlx::query(
            "UPDATE api_keys SET last_used_at = datetime('now', '-30 minutes') WHERE id = ?",
        )
        .bind(row.id)
        .execute(db.pool())
        .await
        .unwrap();

        assert_eq!(
            db.reenable_stale_keys(0).await.expect("reenable"),
            0,
            "0 must not disable the backoff: a 30-minute-old row stays off"
        );
    }

    /// A negative hours value used to form `datetime('now', '--1 hours')`,
    /// which SQLite evaluates to NULL — a silent no-op matching nothing. It
    /// must now behave exactly like the 1h floor, not like "disabled".
    #[tokio::test]
    async fn reenable_stale_keys_treats_negative_as_the_floor_not_a_no_op() {
        let db = crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let fresh = db
            .insert_api_key("tavily", "tvly-floor-neg-fresh")
            .await
            .unwrap();
        let stale = db
            .insert_api_key("tavily", "tvly-floor-neg-stale")
            .await
            .unwrap();
        for (id, offset) in [(fresh.id, "-30 minutes"), (stale.id, "-2 hours")] {
            db.set_api_key_active(id, false).await.unwrap();
            sqlx::query("UPDATE api_keys SET last_used_at = datetime('now', ?) WHERE id = ?")
                .bind(offset)
                .bind(id)
                .execute(db.pool())
                .await
                .unwrap();
        }

        assert_eq!(
            db.reenable_stale_keys(-1).await.expect("reenable"),
            1,
            "negative hours must clamp to the floor, not silently match nothing"
        );
        assert_eq!(
            db.get_api_key(fresh.id).await.unwrap().unwrap().active,
            0,
            "a 30-minute-old row is inside the 1h window"
        );
        assert_eq!(
            db.get_api_key(stale.id).await.unwrap().unwrap().active,
            1,
            "a 2-hour-old row is outside it"
        );
    }
}
