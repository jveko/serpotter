use crate::DbError;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq)]
pub struct ApiKeyRow {
    pub id: i64,
    pub service: String,
    pub key: String,
    pub active: i64,
    pub consecutive_fails: i64,
    /// sha256 hex of the raw key, written on insert and key rotation.
    pub key_fingerprint: String,
    /// Last daily-probe stamp (`date('now')` string), NULL = never probed.
    pub last_probe_at: Option<String>,
}

/// Admin list/detail row with credits + inflight (not used on acquire paths).
#[derive(Clone, Debug, PartialEq)]
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
    pub last_used_at: Option<String>,
    /// Why an inactive row is inactive. Full disposition table in
    /// `crates/serpotter-db/AGENTS.md`:
    /// - `'vendor_suspended'` — the vendor deactivated the account
    ///   (permanently out of rotation since schema 18; the re-enable cron
    ///   skips it, so only an operator toggle brings it back).
    /// - `'manual'` — **operator toggle ONLY** (or a row disabled before
    ///   schema 18, which migration 0018's backfill labelled `'manual'`
    ///   regardless of cause — read `consecutive_fails` to tell a pre-18
    ///   fail@3 row from a real operator toggle).
    /// - `'auth_fail'` — the fail@3 auth hard-disable, stamped by
    ///   `report_api_key_failure_lease` / `note_key_health_failure` in the
    ///   same UPDATE that sets `active = 0` at `MAX_CONSECUTIVE_FAILURES`,
    ///   and only when the reason is still `NULL` — the stamp can never
    ///   fire without the flip, and it can never downgrade a
    ///   `'vendor_suspended'` written by a racing leg. The
    ///   `KEY_REENABLE_AFTER_HOURS` cron does NOT skip this reason: for an
    ///   auth-failed row the revival *is* the recovery path.
    /// - `NULL` — never disabled / re-enabled. An INACTIVE NULL row with
    ///   `consecutive_fails >= 3` is a **pre-0021 legacy** fail@3 disable,
    ///   stamped before this reason existed (a later migration backfills
    ///   those to `'auth_fail'`); an INACTIVE NULL row with fewer fails was
    ///   never disabled by code. This corrects
    ///   `0018_key_disabled_reason.sql:12-13`, whose header claims
    ///   fail@3 = `'manual'`; that file is frozen by checksum, so the
    ///   code's behaviour above is the contract.
    ///
    /// Only present on the admin row; the acquire paths never read it.
    pub disabled_reason: Option<String>,
}

pub(crate) fn map_api_key_admin_row(
    r: &sqlx::sqlite::SqliteRow,
) -> Result<ApiKeyAdminRow, DbError> {
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
        last_used_at: r.try_get("last_used_at")?,
        disabled_reason: r.try_get("disabled_reason")?,
    })
}
