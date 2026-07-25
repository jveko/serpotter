//! SQLite pool + migrations for Serpotter.

mod admin_auth;
mod error;
mod keys;
mod nodes;
mod request_log;
mod settings;
mod stats;
mod tokens;

pub use admin_auth::{AdminSessionRow, AdminUserRow};
pub use error::DbError;
pub use keys::{ApiKeyAdminRow, ApiKeyRow};
pub use request_log::RequestLogRow;
pub use nodes::NodeRow;
pub use stats::ServiceStats;
pub use tokens::TokenRow;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

pub const EXPECTED_SCHEMA_VERSION: i64 = 9;
/// Shared multi-hold deadline default used by keypool (seconds).
/// `lease_until` is a hold expiry for reclaim of abandoned inflight, not exclusive mutex.
pub const KEY_HOLD_TTL_SECS: i64 = 90;
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

/// Open SQLite at `database_url`, run embedded migrations, return pool wrapper.
pub async fn connect_and_migrate(database_url: &str) -> Result<Db, DbError> {
    let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    let max_connections = if database_url.contains(":memory:") { 1 } else { 5 };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(Db { pool })
}
