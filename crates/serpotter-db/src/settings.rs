use crate::{Db, DbError};
use sqlx::Row;

impl Db {
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
}
