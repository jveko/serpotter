//! SQLite pool + migrations for Serpotter.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use thiserror::Error;

pub const EXPECTED_SCHEMA_VERSION: i64 = 2;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

#[derive(Clone, Debug)]
pub struct Db {
    pool: SqlitePool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRow {
    pub id: i64,
    pub token: String,
    pub name: String,
    pub created_at: String,
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

    pub async fn insert_token(&self, token: &str, name: &str) -> Result<TokenRow, DbError> {
        let result = sqlx::query(
            "INSERT INTO tokens (token, name) VALUES (?, ?) RETURNING id, token, name, created_at",
        )
        .bind(token)
        .bind(name)
        .fetch_one(&self.pool)
        .await?;

        Ok(TokenRow {
            id: result.try_get("id")?,
            token: result.try_get("token")?,
            name: result.try_get("name")?,
            created_at: result.try_get("created_at")?,
        })
    }

    pub async fn get_token_by_value(&self, token: &str) -> Result<Option<TokenRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, token, name, created_at FROM tokens WHERE token = ?",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some(r) => Some(TokenRow {
                id: r.try_get("id")?,
                token: r.try_get("token")?,
                name: r.try_get("name")?,
                created_at: r.try_get("created_at")?,
            }),
            None => None,
        })
    }

    pub async fn delete_token_by_id(&self, id: i64) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM tokens WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

/// Open SQLite at `database_url`, run embedded migrations, return pool wrapper.
pub async fn connect_and_migrate(database_url: &str) -> Result<Db, DbError> {
    let options = SqliteConnectOptions::from_str(database_url)?.create_if_missing(true);
    // In-memory SQLite is per-connection; keep max 1 so migrate + queries share the same DB.
    let max_connections = if database_url.contains(":memory:") { 1 } else { 5 };
    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .connect_with(options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(Db { pool })
}
