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
