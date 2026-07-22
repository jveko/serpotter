use crate::{Db, DbError};
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRow {
    pub id: i64,
    pub token: String,
    pub name: String,
    pub created_at: String,
}

impl Db {
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

    pub async fn count_tokens(&self) -> Result<i64, DbError> {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM tokens")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("c")?)
    }
}
