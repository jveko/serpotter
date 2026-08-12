use crate::{Db, DbError};
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminUserRow {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminSessionRow {
    pub token: String,
    pub user_id: i64,
    pub expires_at: String,
    pub created_at: String,
}

impl Db {
    pub async fn count_admin_users(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM admin_users")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }

    /// All admin users (bootstrap normally creates exactly one; the API layer
    /// uses this to verify a current password against any registered user).
    pub async fn list_admin_users(&self) -> Result<Vec<AdminUserRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, username, password_hash, created_at FROM admin_users \
             ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(AdminUserRow {
                id: r.try_get("id")?,
                username: r.try_get("username")?,
                password_hash: r.try_get("password_hash")?,
                created_at: r.try_get("created_at")?,
            });
        }
        Ok(out)
    }

    /// Replace the password hash of one admin user. Returns false when the
    /// user id does not exist.
    pub async fn update_admin_password_hash(
        &self,
        user_id: i64,
        new_hash: &str,
    ) -> Result<bool, DbError> {
        let result = sqlx::query("UPDATE admin_users SET password_hash = ? WHERE id = ?")
            .bind(new_hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn insert_admin_user(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<AdminUserRow, DbError> {
        let row = sqlx::query(
            "INSERT INTO admin_users (username, password_hash) VALUES (?, ?) \
             RETURNING id, username, password_hash, created_at",
        )
        .bind(username)
        .bind(password_hash)
        .fetch_one(&self.pool)
        .await?;
        Ok(AdminUserRow {
            id: row.try_get("id")?,
            username: row.try_get("username")?,
            password_hash: row.try_get("password_hash")?,
            created_at: row.try_get("created_at")?,
        })
    }

    pub async fn get_admin_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<AdminUserRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, username, password_hash, created_at FROM admin_users WHERE username = ?",
        )
        .bind(username)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(AdminUserRow {
                id: r.try_get("id")?,
                username: r.try_get("username")?,
                password_hash: r.try_get("password_hash")?,
                created_at: r.try_get("created_at")?,
            }),
            None => None,
        })
    }

    pub async fn insert_admin_session(
        &self,
        token: &str,
        user_id: i64,
        expires_at: &str,
    ) -> Result<AdminSessionRow, DbError> {
        let row = sqlx::query(
            "INSERT INTO admin_sessions (token, user_id, expires_at) VALUES (?, ?, ?) \
             RETURNING token, user_id, expires_at, created_at",
        )
        .bind(token)
        .bind(user_id)
        .bind(expires_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(AdminSessionRow {
            token: row.try_get("token")?,
            user_id: row.try_get("user_id")?,
            expires_at: row.try_get("expires_at")?,
            created_at: row.try_get("created_at")?,
        })
    }

    /// Valid unexpired session by token, or None.
    pub async fn get_valid_admin_session(
        &self,
        token: &str,
    ) -> Result<Option<AdminSessionRow>, DbError> {
        let row = sqlx::query(
            "SELECT token, user_id, expires_at, created_at FROM admin_sessions \
             WHERE token = ? AND expires_at > datetime('now')",
        )
        .bind(token)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(AdminSessionRow {
                token: r.try_get("token")?,
                user_id: r.try_get("user_id")?,
                expires_at: r.try_get("expires_at")?,
                created_at: r.try_get("created_at")?,
            }),
            None => None,
        })
    }

    pub async fn delete_admin_session(&self, token: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM admin_sessions WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// All sessions (newest first). No password hashes live on sessions;
    /// callers expose these rows to the admin SPA for revocation.
    pub async fn list_admin_sessions(&self) -> Result<Vec<AdminSessionRow>, DbError> {
        let rows = sqlx::query(
            "SELECT token, user_id, expires_at, created_at FROM admin_sessions \
             ORDER BY created_at DESC, token ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(AdminSessionRow {
                token: r.try_get("token")?,
                user_id: r.try_get("user_id")?,
                expires_at: r.try_get("expires_at")?,
                created_at: r.try_get("created_at")?,
            });
        }
        Ok(out)
    }

    /// Revoke one session by token (the admin_sessions primary key).
    /// Returns false when the token is unknown.
    pub async fn revoke_admin_session(&self, token: &str) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM admin_sessions WHERE token = ?")
            .bind(token)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Revoke every session except `keep_token` (None revokes all), e.g. after
    /// a password change so other logged-in browsers lose access while the
    /// current session stays alive. Returns rows affected.
    pub async fn revoke_admin_sessions_except(
        &self,
        keep_token: Option<&str>,
    ) -> Result<i64, DbError> {
        let result = match keep_token {
            Some(tok) => {
                sqlx::query("DELETE FROM admin_sessions WHERE token != ?")
                    .bind(tok)
                    .execute(&self.pool)
                    .await?
            }
            None => {
                sqlx::query("DELETE FROM admin_sessions")
                    .execute(&self.pool)
                    .await?
            }
        };
        Ok(result.rows_affected() as i64)
    }

    /// Remove all sessions whose expiry has passed, returning rows affected.
    /// Called by the maintenance loop so stale sessions cannot accrue.
    pub async fn purge_expired_admin_sessions(&self) -> Result<i64, DbError> {
        let result = sqlx::query("DELETE FROM admin_sessions WHERE expires_at < datetime('now')")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() as i64)
    }

    /// SQLite `datetime('now', '+N days')` for session expiry stamps.
    pub async fn datetime_now_plus_days(&self, days: i64) -> Result<String, DbError> {
        let row = sqlx::query("SELECT datetime('now', '+' || ? || ' days') AS e")
            .bind(days)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("e")?)
    }
}
