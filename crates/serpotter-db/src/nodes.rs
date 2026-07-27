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
    pub consecutive_fails: i64,
    pub last_error: Option<String>,
}

fn map_node_row(r: &sqlx::sqlite::SqliteRow) -> Result<NodeRow, DbError> {
    Ok(NodeRow {
        id: r.try_get("id")?,
        host: r.try_get("host")?,
        port: r.try_get("port")?,
        username: r.try_get("username")?,
        password: r.try_get("password")?,
        enabled: r.try_get("enabled")?,
        inflight: r.try_get("inflight")?,
        consecutive_fails: r.try_get("consecutive_fails")?,
        last_error: r.try_get("last_error")?,
    })
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
             RETURNING id, host, port, username, password, enabled, inflight, consecutive_fails, last_error",
        )
        .bind(host)
        .bind(port)
        .bind(username)
        .bind(password)
        .fetch_one(&self.pool)
        .await?;
        map_node_row(&result)
    }

    pub async fn list_nodes(&self) -> Result<Vec<NodeRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, host, port, username, password, enabled, inflight, consecutive_fails, last_error \
             FROM nodes ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(map_node_row(&r)?);
        }
        Ok(out)
    }

    pub async fn get_node(&self, id: i64) -> Result<Option<NodeRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, host, port, username, password, enabled, inflight, consecutive_fails, last_error \
             FROM nodes WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(map_node_row(&r)?),
            None => None,
        })
    }

    /// Atomic least-inflight pick + inflight bump in one statement.
    /// Subquery UPDATE + RETURNING serializes pick-and-bump so concurrent
    /// connections cannot double-pick the same least-inflight row.
    pub async fn acquire_outbound_node(&self) -> Result<Option<NodeRow>, DbError> {
        let row = sqlx::query(
            "UPDATE nodes SET inflight = inflight + 1 \
             WHERE id = ( \
               SELECT id FROM nodes \
               WHERE enabled = 1 \
               ORDER BY inflight ASC, id ASC \
               LIMIT 1 \
             ) \
             RETURNING id, host, port, username, password, enabled, inflight, consecutive_fails, last_error",
        )
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(map_node_row(&r)?),
            None => None,
        })
    }

    pub async fn release_node_inflight(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE nodes SET inflight = MAX(0, inflight - 1) WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Success: reset consecutive_fails, clear last_error, release one inflight.
    pub async fn report_node_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE nodes SET \
                consecutive_fails = 0, \
                last_error = NULL, \
                inflight = MAX(0, inflight - 1) \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Failure: bump consecutive_fails, store last_error, disable at max_fails, release one inflight.
    pub async fn report_node_failure(
        &self,
        id: i64,
        max_fails: i64,
        last_error: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE nodes SET \
                consecutive_fails = consecutive_fails + 1, \
                last_error = ?, \
                inflight = MAX(0, inflight - 1), \
                enabled = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE enabled END \
             WHERE id = ?",
        )
        .bind(last_error)
        .bind(max_fails)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn zero_all_node_inflight(&self) -> Result<(), DbError> {
        sqlx::query("UPDATE nodes SET inflight = 0")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn bump_node_inflight(&self, id: i64, delta: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE nodes SET inflight = MAX(0, inflight + ?) WHERE id = ?")
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
