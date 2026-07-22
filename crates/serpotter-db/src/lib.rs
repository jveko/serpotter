//! SQLite pool + migrations for Serpotter.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use thiserror::Error;

pub const EXPECTED_SCHEMA_VERSION: i64 = 7;
pub const LEASE_TTL_SECS: i64 = 20;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRow {
    pub id: i64,
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub enabled: i64,
    pub inflight: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceStats {
    pub service: String,
    pub keys: i64,
    pub active: i64,
    pub credits_remaining_sum: Option<i64>,
    pub credits_limit_sum: Option<i64>,
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

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        let row = sqlx::query("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match row {
            Some(r) => Some(r.try_get("value")?),
            None => None,
        })
    }

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, datetime('now')) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_social_enabled(&self) -> Result<bool, DbError> {
        Ok(match self.get_setting("social_enabled").await? {
            Some(v) => v == "true" || v == "1",
            None => true,
        })
    }

    pub async fn set_social_enabled(&self, enabled: bool) -> Result<(), DbError> {
        self.set_setting("social_enabled", if enabled { "true" } else { "false" })
            .await
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

    /// Pick least-recently-used active key for service (credit priority then LRU).
    /// Skips keys with an unexpired soft lease; stamps `lease_until` on pick.
    pub async fn acquire_api_key(&self, service: &str) -> Result<Option<ApiKeyRow>, DbError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 \
               AND (lease_until IS NULL OR lease_until <= datetime('now')) \
             ORDER BY \
               CASE WHEN credits_remaining IS NULL OR credits_remaining > 0 THEN 1 ELSE 2 END, \
               last_used_at IS NOT NULL, \
               last_used_at ASC, \
               id ASC \
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
        sqlx::query(
            "UPDATE api_keys SET \
                last_used_at = datetime('now'), \
                lease_until = datetime('now', '+' || ? || ' seconds') \
             WHERE id = ?",
        )
        .bind(LEASE_TTL_SECS)
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

    /// Acquire up to `n` distinct healthy keys (n clamped to 1..=10) in one transaction.
    /// Credit priority then LRU; zero-credit keys remain eligible as priority 2.
    /// Skips unexpired leases; stamps `lease_until` on each pick.
    pub async fn acquire_api_keys_batch(
        &self,
        service: &str,
        n: usize,
    ) -> Result<Vec<ApiKeyRow>, DbError> {
        let n = n.clamp(1, 10) as i64;
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 \
               AND (lease_until IS NULL OR lease_until <= datetime('now')) \
             ORDER BY \
               CASE WHEN credits_remaining IS NULL OR credits_remaining > 0 THEN 1 ELSE 2 END, \
               last_used_at IS NOT NULL, \
               last_used_at ASC, \
               id ASC \
             LIMIT ?",
        )
        .bind(service)
        .bind(n)
        .fetch_all(&mut *tx)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let id: i64 = r.try_get("id")?;
            sqlx::query(
                "UPDATE api_keys SET \
                    last_used_at = datetime('now'), \
                    lease_until = datetime('now', '+' || ? || ' seconds') \
                 WHERE id = ?",
            )
            .bind(LEASE_TTL_SECS)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            out.push(ApiKeyRow {
                id,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
            });
        }

        tx.commit().await?;
        Ok(out)
    }

    pub async fn report_api_key_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = 0, \
                last_used_at = datetime('now'), \
                lease_until = NULL \
             WHERE id = ?",
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
                lease_until = NULL, \
                active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END \
             WHERE id = ?",
        )
        .bind(MAX_CONSECUTIVE_FAILURES)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Zero credits (mysearch parity). Does NOT set active=0; hard-disable is fail@3 only.
    /// Clears soft lease so the key is eligible again as priority-2.
    pub async fn report_api_key_exhausted(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                credits_remaining = 0, \
                last_used_at = datetime('now'), \
                lease_until = NULL \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Test helper: force `lease_until` (ISO-ish SQLite datetime text, or NULL).
    pub async fn set_api_key_lease_until(
        &self,
        id: i64,
        lease_until: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET lease_until = ? WHERE id = ?")
            .bind(lease_until)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
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

    /// Active keys for a service, never-synced first then oldest sync.
    pub async fn list_active_keys_for_service(
        &self,
        service: &str,
    ) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 \
             ORDER BY usage_synced_at IS NOT NULL, usage_synced_at ASC, id ASC",
        )
        .bind(service)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
            });
        }
        Ok(out)
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

    pub async fn list_tokens(&self) -> Result<Vec<TokenRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, token, name, created_at FROM tokens ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(TokenRow {
                id: r.try_get("id")?,
                token: r.try_get("token")?,
                name: r.try_get("name")?,
                created_at: r.try_get("created_at")?,
            });
        }
        Ok(out)
    }

    pub async fn list_api_keys(&self) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
            });
        }
        Ok(out)
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

    pub async fn count_tokens(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM tokens")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
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

    pub async fn count_nodes(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM nodes")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }

    pub async fn insert_node(
        &self,
        host: &str,
        port: i64,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<NodeRow, DbError> {
        let result = sqlx::query(
            "INSERT INTO nodes (host, port, username, password) VALUES (?, ?, ?, ?) \
             RETURNING id, host, port, username, password, enabled, inflight",
        )
        .bind(host)
        .bind(port)
        .bind(username)
        .bind(password)
        .fetch_one(&self.pool)
        .await?;
        Ok(NodeRow {
            id: result.try_get("id")?,
            host: result.try_get("host")?,
            port: result.try_get("port")?,
            username: result.try_get("username")?,
            password: result.try_get("password")?,
            enabled: result.try_get("enabled")?,
            inflight: result.try_get("inflight")?,
        })
    }

    pub async fn list_nodes(&self) -> Result<Vec<NodeRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, host, port, username, password, enabled, inflight FROM nodes ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(NodeRow {
                id: r.try_get("id")?,
                host: r.try_get("host")?,
                port: r.try_get("port")?,
                username: r.try_get("username")?,
                password: r.try_get("password")?,
                enabled: r.try_get("enabled")?,
                inflight: r.try_get("inflight")?,
            });
        }
        Ok(out)
    }

    /// Least-inflight enabled node, if any.
    pub async fn select_outbound_node(&self) -> Result<Option<NodeRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, host, port, username, password, enabled, inflight FROM nodes \
             WHERE enabled = 1 ORDER BY inflight ASC, id ASC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(NodeRow {
                id: r.try_get("id")?,
                host: r.try_get("host")?,
                port: r.try_get("port")?,
                username: r.try_get("username")?,
                password: r.try_get("password")?,
                enabled: r.try_get("enabled")?,
                inflight: r.try_get("inflight")?,
            }),
            None => None,
        })
    }

    pub async fn bump_node_inflight(&self, id: i64, delta: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE nodes SET inflight = MAX(0, inflight + ?) WHERE id = ?",
        )
        .bind(delta)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_node_enabled(&self, id: i64, enabled: bool) -> Result<bool, DbError> {
        let result = sqlx::query("UPDATE nodes SET enabled = ? WHERE id = ?")
            .bind(if enabled { 1i64 } else { 0i64 })
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_node(&self, id: i64) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM nodes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn insert_request_log(
        &self,
        path: &str,
        method: &str,
        status: i64,
        service: Option<&str>,
        provider_used: Option<&str>,
        duration_ms: Option<i64>,
        error_kind: Option<&str>,
        query_preview: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "INSERT INTO request_log \
             (path, method, status, service, provider_used, duration_ms, error_kind, query_preview) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(path)
        .bind(method)
        .bind(status)
        .bind(service)
        .bind(provider_used)
        .bind(duration_ms)
        .bind(error_kind)
        .bind(query_preview)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete logs older than `retention_days`, then cap total rows to `max_rows` (oldest first).
    pub async fn purge_request_log(
        &self,
        retention_days: i64,
        max_rows: i64,
    ) -> Result<u64, DbError> {
        let days = retention_days.max(0);
        let max_rows = max_rows.max(0);
        let aged = sqlx::query(
            "DELETE FROM request_log WHERE created_at < datetime('now', '-' || ? || ' days')",
        )
        .bind(days)
        .execute(&self.pool)
        .await?
        .rows_affected();

        let capped = if max_rows == 0 {
            sqlx::query("DELETE FROM request_log")
                .execute(&self.pool)
                .await?
                .rows_affected()
        } else {
            // Keep the newest max_rows; delete the rest (oldest first via OFFSET).
            sqlx::query(
                "DELETE FROM request_log WHERE id IN (
                    SELECT id FROM request_log
                    ORDER BY created_at ASC, id ASC
                    LIMIT -1 OFFSET ?
                )",
            )
            .bind(max_rows)
            .execute(&self.pool)
            .await?
            .rows_affected()
        };
        Ok(aged + capped)
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

    /// Test helper: force `last_used_at` (SQLite datetime text).
    pub async fn set_api_key_last_used_at(
        &self,
        id: i64,
        last_used_at: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
            .bind(last_used_at)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn count_request_logs(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM request_log")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }

    pub async fn stats_by_service(&self) -> Result<Vec<ServiceStats>, DbError> {
        let rows = sqlx::query(
            "SELECT service, \
                    COUNT(*) AS keys, \
                    COALESCE(SUM(CASE WHEN active = 1 THEN 1 ELSE 0 END), 0) AS active, \
                    SUM(credits_remaining) AS credits_remaining_sum, \
                    SUM(credits_limit) AS credits_limit_sum \
             FROM api_keys \
             GROUP BY service \
             ORDER BY service ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ServiceStats {
                service: r.try_get("service")?,
                keys: r.try_get("keys")?,
                active: r.try_get("active")?,
                credits_remaining_sum: r.try_get("credits_remaining_sum")?,
                credits_limit_sum: r.try_get("credits_limit_sum")?,
            });
        }
        Ok(out)
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
