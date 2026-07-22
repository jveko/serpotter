use crate::{Db, DbError};
use sqlx::Row;

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

impl Db {
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
}
