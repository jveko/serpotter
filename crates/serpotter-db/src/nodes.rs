use crate::{Db, DbError, NODE_HOLD_TTL_SECS};
use sqlx::Row;

/// Reclaim UPDATE for `nodes` — single source of truth shared by the public
/// helper and the acquire-path transaction (see [`Db::reclaim_expired_holds`]).
const RECLAIM_NODES_SQL: &str = "UPDATE nodes SET inflight = 0, lease_until = NULL \
     WHERE lease_until IS NOT NULL AND lease_until <= datetime('now')";

/// Wire/storage allowlist for `nodes.protocol`.
pub fn is_allowed_node_protocol(protocol: &str) -> bool {
    matches!(protocol, "http" | "https" | "socks5")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeRow {
    pub id: i64,
    pub host: String,
    pub port: i64,
    pub protocol: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub enabled: i64,
    pub inflight: i64,
    pub consecutive_fails: i64,
    pub last_error: Option<String>,
    pub lease_until: Option<String>,
    /// When the node was last disabled (NULL = enabled or never disabled);
    /// the maintenance cron auto re-enables after NODE_REENABLE_AFTER_HOURS.
    pub disabled_at: Option<String>,
}

fn map_node_row(r: &sqlx::sqlite::SqliteRow) -> Result<NodeRow, DbError> {
    Ok(NodeRow {
        id: r.try_get("id")?,
        host: r.try_get("host")?,
        port: r.try_get("port")?,
        protocol: r.try_get("protocol")?,
        username: r.try_get("username")?,
        password: r.try_get("password")?,
        enabled: r.try_get("enabled")?,
        inflight: r.try_get("inflight")?,
        consecutive_fails: r.try_get("consecutive_fails")?,
        last_error: r.try_get("last_error")?,
        lease_until: r.try_get("lease_until")?,
        disabled_at: r.try_get("disabled_at")?,
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
        protocol: &str,
    ) -> Result<NodeRow, DbError> {
        debug_assert!(
            crate::is_allowed_node_protocol(protocol),
            "protocol must be http|https|socks5 (admin validates)"
        );
        let result = sqlx::query(
            "INSERT INTO nodes (host, port, username, password, protocol) VALUES (?, ?, ?, ?, ?) \
             RETURNING id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until, disabled_at",
        )
        .bind(host)
        .bind(port)
        .bind(username)
        .bind(password)
        .bind(protocol)
        .fetch_one(&self.pool)
        .await?;
        map_node_row(&result)
    }

    pub async fn list_nodes(&self) -> Result<Vec<NodeRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until, disabled_at \
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
            "SELECT id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until, disabled_at \
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

    /// Patch a node's connection settings without re-creating the row.
    /// `host` / `port` / `protocol` are optional (absent = keep current);
    /// `username` / `password` are `Option<Option<&str>>` so a caller can
    /// keep (`None`), clear (`Some(None)` → NULL), or set (`Some(Some(v))`).
    /// Enabled / inflight / failure state is never touched here — only the
    /// admin-editable connection fields change. Returns `None` when the id
    /// does not exist. The admin layer guarantees at least one field.
    pub async fn update_node(
        &self,
        id: i64,
        host: Option<&str>,
        port: Option<i64>,
        protocol: Option<&str>,
        username: Option<Option<&str>>,
        password: Option<Option<&str>>,
    ) -> Result<Option<NodeRow>, DbError> {
        use sqlx::{QueryBuilder, Sqlite};

        // Join only the supplied fields. `push` applies the ", " separator to
        // the next fragment; `push_bind_unseparated` attaches the value to its
        // column so the `?` count always matches the bind list.
        let mut qb: QueryBuilder<Sqlite> = QueryBuilder::new("UPDATE nodes SET ");
        let mut sets = qb.separated(", ");
        if let Some(h) = host {
            sets.push("host = ").push_bind_unseparated(h);
        }
        if let Some(p) = port {
            sets.push("port = ").push_bind_unseparated(p);
        }
        if let Some(proto) = protocol {
            sets.push("protocol = ").push_bind_unseparated(proto);
        }
        if let Some(u) = username {
            // Some(None) binds NULL (clear); Some(Some(v)) binds the value.
            sets.push("username = ").push_bind_unseparated(u);
        }
        if let Some(pw) = password {
            sets.push("password = ").push_bind_unseparated(pw);
        }
        // `sets` is done — NLL releases the mutable borrow of `qb` here.
        qb.push(" WHERE id = ").push_bind(id);
        qb.push(
            " RETURNING id, host, port, protocol, username, password, enabled, \
             inflight, consecutive_fails, last_error, lease_until, disabled_at",
        );

        let row = qb.build().fetch_optional(&self.pool).await?;
        Ok(match row {
            Some(r) => Some(map_node_row(&r)?),
            None => None,
        })
    }

    /// Zero inflight and clear lease when hold deadline has passed.
    pub async fn reclaim_expired_node_holds(&self) -> Result<u64, DbError> {
        Db::reclaim_expired_holds(&self.pool, RECLAIM_NODES_SQL).await
    }

    /// Atomic least-inflight pick + inflight bump + lease stamp.
    /// Reclaims expired holds first (keys parity). Uses [`NODE_HOLD_TTL_SECS`].
    pub async fn acquire_outbound_node(&self) -> Result<Option<NodeRow>, DbError> {
        self.acquire_outbound_node_with_ttl(NODE_HOLD_TTL_SECS)
            .await
    }

    /// Same as [`acquire_outbound_node`] with explicit hold TTL (seconds, min 1).
    pub async fn acquire_outbound_node_with_ttl(
        &self,
        hold_ttl_secs: i64,
    ) -> Result<Option<NodeRow>, DbError> {
        let hold_ttl_secs = hold_ttl_secs.max(1);
        let mut tx = self.pool.begin().await?;
        Db::reclaim_expired_holds(&mut *tx, RECLAIM_NODES_SQL).await?;

        let row = sqlx::query(
            "UPDATE nodes SET \
                inflight = inflight + 1, \
                lease_until = datetime('now', '+' || ? || ' seconds') \
             WHERE id = ( \
               SELECT id FROM nodes \
               WHERE enabled = 1 \
               ORDER BY inflight ASC, id ASC \
               LIMIT 1 \
             ) \
             RETURNING id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until, disabled_at",
        )
        .bind(hold_ttl_secs)
        .fetch_optional(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(match row {
            Some(r) => Some(map_node_row(&r)?),
            None => None,
        })
    }

    /// Multi-hold-safe release: decrement inflight; clear lease_until only when now 0.
    pub async fn release_node_inflight(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE nodes SET \
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END \
             WHERE id = ?",
        )
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
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Failure: bump consecutive_fails, store last_error, disable at max_fails
    /// (stamping `disabled_at` so the cron can auto re-enable later), release one inflight.
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
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END, \
                enabled = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE enabled END, \
                disabled_at = CASE WHEN consecutive_fails + 1 >= ? THEN datetime('now') ELSE disabled_at END \
             WHERE id = ?",
        )
        .bind(last_error)
        .bind(max_fails)
        .bind(max_fails)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn zero_all_node_inflight(&self) -> Result<(), DbError> {
        sqlx::query("UPDATE nodes SET inflight = 0, lease_until = NULL")
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

    /// Toggle enabled. On re-enable (`enabled=true`), clear consecutive_fails,
    /// last_error, and disabled_at so admin Toggle does not immediately
    /// re-disable on the next report (keys parity). On disable, stamp
    /// `disabled_at = now` so the cron can auto re-enable after the recovery
    /// window (NODE_REENABLE_AFTER_HOURS).
    pub async fn set_node_enabled(&self, id: i64, enabled: bool) -> Result<bool, DbError> {
        let flag = if enabled { 1i64 } else { 0i64 };
        let result = sqlx::query(
            "UPDATE nodes SET \
                enabled = ?, \
                consecutive_fails = CASE WHEN ? = 1 THEN 0 ELSE consecutive_fails END, \
                last_error = CASE WHEN ? = 1 THEN NULL ELSE last_error END, \
                disabled_at = CASE WHEN ? = 1 THEN NULL ELSE datetime('now') END \
             WHERE id = ?",
        )
        .bind(flag)
        .bind(flag)
        .bind(flag)
        .bind(flag)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Re-stamp `lease_until` for a still-held node (long polls refresh their
    /// lease mid-call so it never expires under an in-flight hold). The
    /// `inflight > 0` guard makes it a true no-op for released/absent nodes —
    /// a released lease is never re-stamped (the caller's release already
    /// cleared it). Refresh is best-effort: errors never panic or fail the
    /// poll loop.
    pub async fn refresh_node_lease(&self, id: i64, hold_ttl_secs: i64) -> Result<(), DbError> {
        let ttl = hold_ttl_secs.max(1);
        sqlx::query(
            "UPDATE nodes SET lease_until = datetime('now', '+' || ? || ' seconds') \
             WHERE id = ? AND inflight > 0",
        )
        .bind(ttl)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Re-enable nodes that have been disabled for at least `hours` (measured
    /// from `disabled_at`, stamped whenever a node was disabled). Clears
    /// consecutive_fails / last_error / disabled_at (keys parity via
    /// [`Db::reenable_stale_keys`]). Returns rows affected.
    pub async fn reenable_stale_nodes(&self, hours: i64) -> Result<u64, DbError> {
        let hours = hours.max(0);
        let result = sqlx::query(
            "UPDATE nodes SET enabled = 1, consecutive_fails = 0, last_error = NULL, disabled_at = NULL \
             WHERE enabled = 0 \
               AND disabled_at IS NOT NULL \
               AND disabled_at <= datetime('now', '-' || ? || ' hours')",
        )
        .bind(hours)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_node(&self, id: i64) -> Result<bool, DbError> {
        let result = sqlx::query("DELETE FROM nodes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
