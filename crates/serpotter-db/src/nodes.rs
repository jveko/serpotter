use crate::{Db, DbError, NODE_HOLD_TTL_SECS};
use sqlx::Row;

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

#[derive(Clone, Debug)]
pub struct NodeLease {
    pub node: NodeRow,
    pub token: i64,
}

impl std::ops::Deref for NodeLease {
    type Target = NodeRow;
    fn deref(&self) -> &Self::Target {
        &self.node
    }
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

    pub async fn reclaim_expired_node_holds(&self) -> Result<u64, DbError> {
        let mut tx = self.pool.begin().await?;
        let removed = sqlx::query("DELETE FROM node_leases WHERE lease_until <= datetime('now')")
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query(
            "UPDATE nodes SET inflight = (SELECT COUNT(*) FROM node_leases WHERE node_id = nodes.id), \
             lease_until = (SELECT MAX(lease_until) FROM node_leases WHERE node_id = nodes.id) \
             WHERE inflight != (SELECT COUNT(*) FROM node_leases WHERE node_id = nodes.id) \
                OR lease_until IS NOT (SELECT MAX(lease_until) FROM node_leases WHERE node_id = nodes.id)",
        ).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(removed)
    }

    pub async fn acquire_outbound_node(&self) -> Result<Option<NodeLease>, DbError> {
        self.acquire_outbound_node_with_ttl(NODE_HOLD_TTL_SECS)
            .await
    }

    pub async fn acquire_outbound_node_with_ttl(
        &self,
        hold_ttl_secs: i64,
    ) -> Result<Option<NodeLease>, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM node_leases WHERE lease_until <= datetime('now')")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE nodes SET inflight = (SELECT COUNT(*) FROM node_leases WHERE node_id = nodes.id), \
             lease_until = (SELECT MAX(lease_until) FROM node_leases WHERE node_id = nodes.id) \
             WHERE inflight != (SELECT COUNT(*) FROM node_leases WHERE node_id = nodes.id) \
                OR lease_until IS NOT (SELECT MAX(lease_until) FROM node_leases WHERE node_id = nodes.id)",
        ).execute(&mut *tx).await?;
        let row = sqlx::query(
            "UPDATE nodes SET inflight = inflight + 1 \
             WHERE id = (SELECT id FROM nodes WHERE enabled = 1 ORDER BY inflight ASC, id ASC LIMIT 1) \
             RETURNING id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until, disabled_at",
        ).fetch_optional(&mut *tx).await?;
        let lease = if let Some(r) = row {
            let id: i64 = r.try_get("id")?;
            let token: i64 = sqlx::query_scalar(
                "INSERT INTO node_leases(node_id, lease_until) VALUES (?, datetime('now', '+' || ? || ' seconds')) RETURNING token",
            ).bind(id).bind(hold_ttl_secs.max(1)).fetch_one(&mut *tx).await?;
            let row = sqlx::query("UPDATE nodes SET lease_until = (SELECT MAX(lease_until) FROM node_leases WHERE node_id = ?) WHERE id = ? RETURNING id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until, disabled_at")
                .bind(id).bind(id).fetch_one(&mut *tx).await?;
            Some(NodeLease {
                node: map_node_row(&row)?,
                token,
            })
        } else {
            None
        };
        tx.commit().await?;
        Ok(lease)
    }

    async fn release_node_token(
        &self,
        token: i64,
        health: Option<&str>,
        max_fails: i64,
        error: Option<&str>,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let id: Option<i64> =
            sqlx::query_scalar("DELETE FROM node_leases WHERE token = ? RETURNING node_id")
                .bind(token)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(id) = id else {
            tx.commit().await?;
            return Ok(false);
        };
        match health {
            Some("success") => {
                sqlx::query(
                    "UPDATE nodes SET consecutive_fails = 0, last_error = NULL WHERE id = ?",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            Some("failure") => {
                sqlx::query("UPDATE nodes SET consecutive_fails = consecutive_fails + 1, last_error = ?, enabled = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE enabled END, disabled_at = CASE WHEN consecutive_fails + 1 >= ? THEN datetime('now') ELSE disabled_at END WHERE id = ?").bind(error).bind(max_fails).bind(max_fails).bind(id).execute(&mut *tx).await?;
            }
            _ => {}
        }
        sqlx::query("UPDATE nodes SET inflight = (SELECT COUNT(*) FROM node_leases WHERE node_id = ?), lease_until = (SELECT MAX(lease_until) FROM node_leases WHERE node_id = ?) WHERE id = ?")
            .bind(id).bind(id).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn release_node_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_node_token(token, None, crate::MAX_CONSECUTIVE_FAILURES, None)
            .await
    }
    pub async fn report_node_success_lease(&self, token: i64) -> Result<bool, DbError> {
        self.release_node_token(
            token,
            Some("success"),
            crate::MAX_CONSECUTIVE_FAILURES,
            None,
        )
        .await
    }
    pub async fn report_node_failure_lease(
        &self,
        token: i64,
        max_fails: i64,
        error: Option<&str>,
    ) -> Result<bool, DbError> {
        self.release_node_token(token, Some("failure"), max_fails, error)
            .await
    }

    /// Health-only note for callers that do not own a lease token. This never
    /// releases a holder; use `report_node_success_lease` for an outcome.
    pub async fn note_node_health_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE nodes SET consecutive_fails = 0, last_error = NULL WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    /// Health-only note for callers that do not own a lease token. This never
    /// releases a holder; use `report_node_failure_lease` for an outcome.
    pub async fn note_node_health_failure(
        &self,
        id: i64,
        max_fails: i64,
        error: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE nodes SET consecutive_fails = consecutive_fails + 1, last_error = ?, enabled = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE enabled END, disabled_at = CASE WHEN consecutive_fails + 1 >= ? THEN datetime('now') ELSE disabled_at END WHERE id = ?").bind(error).bind(max_fails).bind(max_fails).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn zero_all_node_inflight(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM node_leases")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE nodes SET inflight = 0, lease_until = NULL")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn set_node_enabled(&self, id: i64, enabled: bool) -> Result<bool, DbError> {
        let flag = if enabled { 1i64 } else { 0i64 };
        let result = sqlx::query("UPDATE nodes SET enabled = ?, consecutive_fails = CASE WHEN ? = 1 THEN 0 ELSE consecutive_fails END, last_error = CASE WHEN ? = 1 THEN NULL ELSE last_error END, disabled_at = CASE WHEN ? = 1 THEN NULL ELSE datetime('now') END WHERE id = ?")
            .bind(flag).bind(flag).bind(flag).bind(flag).bind(id).execute(&self.pool).await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn refresh_node_lease(
        &self,
        token: i64,
        hold_ttl_secs: i64,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query("UPDATE node_leases SET lease_until = datetime('now', '+' || ? || ' seconds') WHERE token = ?")
            .bind(hold_ttl_secs.max(1)).bind(token).execute(&mut *tx).await?.rows_affected() > 0;
        if changed {
            sqlx::query("UPDATE nodes SET lease_until = (SELECT MAX(lease_until) FROM node_leases WHERE node_id = nodes.id) WHERE id = (SELECT node_id FROM node_leases WHERE token = ?)")
                .bind(token).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    /// Re-enable nodes that have been disabled for at least `hours` (measured
    /// from `disabled_at`, stamped whenever a node was disabled). Clears
    /// consecutive_fails / last_error / disabled_at (keys parity via
    /// [`Db::reenable_stale_keys`]). Returns rows affected.
    ///
    /// `hours` is clamped to [`Db::REENABLE_MIN_HOURS`], the SAME floor the
    /// key path uses, for the same two reasons: `0` makes the predicate true
    /// for every disabled node (node fail@max backoff is silently off — the
    /// next 15-minute tick hands a just-disabled node straight back), and a
    /// negative value would form `datetime('now', '--1 hours')`, which SQLite
    /// evaluates to NULL and which therefore matches nothing. The clamp is
    /// here, not only at the caller, so the SQL modifier can never take a
    /// `--N` form no matter who calls it; `cron.rs` warns loudly at startup and
    /// `docs/ops/env.md` documents the range.
    pub async fn reenable_stale_nodes(&self, hours: i64) -> Result<u64, DbError> {
        let hours = hours.max(Db::REENABLE_MIN_HOURS);
        let result = sqlx::query(
            "UPDATE nodes SET enabled = 1, consecutive_fails = 0, last_error = NULL, disabled_at = NULL \
             WHERE enabled = 0 AND disabled_at IS NOT NULL AND disabled_at <= datetime('now', '-' || ? || ' hours')",
        ).bind(hours).execute(&self.pool).await?;
        Ok(result.rows_affected())
    }

    pub async fn delete_node(&self, id: i64) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM node_leases WHERE node_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM nodes WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }
}
