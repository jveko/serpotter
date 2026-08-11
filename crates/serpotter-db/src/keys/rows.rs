use crate::DbError;
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyRow {
    pub id: i64,
    pub service: String,
    pub key: String,
    pub active: i64,
    pub consecutive_fails: i64,
    /// sha256 hex of the raw key, written on insert and key rotation.
    pub key_fingerprint: String,
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
    pub last_used_at: Option<String>,
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
    })
}
