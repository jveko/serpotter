//! SQLite pool + migrations for Serpotter.

mod admin_auth;
mod cache;
mod error;
mod jobs;
mod keys;
mod nodes;
mod request_log;
mod settings;
mod stats;
mod tokens;
mod usage;

pub use admin_auth::{AdminSessionRow, AdminUserRow};
pub use cache::CacheRow;
pub use error::DbError;
pub use jobs::ProviderJobRow;
pub use keys::{ApiKeyAdminRow, ApiKeyRow};
pub use nodes::{is_allowed_node_protocol, NodeRow};
pub use request_log::{RequestLogFilter, RequestLogRow};
pub use stats::ServiceStats;
pub use tokens::TokenRow;
pub use usage::{SpendKeyRow, SpendServiceRow, UsageDailyRow};

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

pub const EXPECTED_SCHEMA_VERSION: i64 = 15;
/// Shared multi-hold deadline default used by keypool (seconds).
/// `lease_until` is a hold expiry for reclaim of abandoned inflight, not exclusive mutex.
pub const KEY_HOLD_TTL_SECS: i64 = 90;
/// Integer scale for credit×load score: `(effective_C * SCALE) / (inflight + 1)`.
pub const KEY_CREDIT_SCORE_SCALE: i64 = 1000;
/// Default effective_C when `credits_remaining IS NULL` (Exa/xAI/unsynced).
pub const DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT: i64 = 100;
/// Node multi-hold deadline default used by outbound ProxyPool (seconds).
pub const NODE_HOLD_TTL_SECS: i64 = 90;
pub const MAX_CONSECUTIVE_FAILURES: i64 = 3;

#[derive(Clone, Debug)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<(), DbError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    pub async fn schema_version(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT version FROM schema_version WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("version")?)
    }
}

#[cfg(test)]
impl Db {
    /// Shared in-crate test helper: fresh in-memory migrated DB
    /// (single connection — `:memory:` trap, see CONVENTIONS).
    pub(crate) async fn connect_for_test() -> Db {
        connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate in-memory test db")
    }
}

/// Open SQLite at `database_url`, run embedded migrations, return pool wrapper.
pub async fn connect_and_migrate(database_url: &str) -> Result<Db, DbError> {
    let mut options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    if !database_url.contains(":memory:") {
        // WAL for on-disk DBs: the shared 5-connection pool reads/writes
        // concurrently without reader-writer lock contention, and the pool
        // survives crash recovery better. `:memory:` databases are left on
        // their default journal mode so per-connection in-memory tests
        // behave as before.
        options = options.journal_mode(SqliteJournalMode::Wal);
    }
    let max_connections = if database_url.contains(":memory:") {
        1
    } else {
        5
    };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(Db { pool })
}
