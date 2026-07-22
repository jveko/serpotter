//! SQLite pool + migrations for Serpotter.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use thiserror::Error;

pub const EXPECTED_SCHEMA_VERSION: i64 = 3;
pub const MAX_CONSECUTIVE_FAILURES: i64 = 3;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiKeyRow {
    pub id: i64,
    pub service: String,
    pub key: String,
    pub active: i64,
    pub consecutive_fails: i64,
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

    /// Pick least-recently-used active key for service (lean round-robin).
    pub async fn acquire_api_key(&self, service: &str) -> Result<Option<ApiKeyRow>, DbError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 \
             ORDER BY last_used_at IS NOT NULL, last_used_at ASC, id ASC \
             LIMIT 1",
        )
        .bind(service)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };

        let id: i64 = r.try_get("id")?;
        sqlx::query("UPDATE api_keys SET last_used_at = datetime('now') WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        Ok(Some(ApiKeyRow {
            id,
            service: r.try_get("service")?,
            key: r.try_get("key")?,
            active: r.try_get("active")?,
            consecutive_fails: r.try_get("consecutive_fails")?,
        }))
    }

    pub async fn report_api_key_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET consecutive_fails = 0, last_used_at = datetime('now') WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn report_api_key_failure(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = consecutive_fails + 1, \
                last_used_at = datetime('now'), \
                active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END \
             WHERE id = ?",
        )
        .bind(MAX_CONSECUTIVE_FAILURES)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_api_key(&self, id: i64) -> Result<Option<ApiKeyRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
            }),
            None => None,
        })
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
