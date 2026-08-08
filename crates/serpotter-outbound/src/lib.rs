//! Outbound HTTP(S)/SOCKS proxy URLs and live node rotation for `reqwest::Proxy::all`.
//!
//! Product path: `ProxyPool::acquire` returns a least-inflight `nodes` lease or `None`
//! (direct). Reqwest owns the CONNECT tunnel via `Proxy::all` — no custom dialer.
//! `proxy_url_from_node` builds scheme URLs from `row.protocol`. When `require_proxy`
//! is set, product maps `None` → `NoHealthyNode` (503). xAI always dials direct.

use serpotter_db::{Db, DbError};
use thiserror::Error;
use tokio::sync::Mutex;

/// Build `{protocol}://[user:pass@]host:port` for `reqwest::Proxy::all`.
/// `protocol` must already be allowlisted (`http`|`https`|`socks5`).
pub fn proxy_url_from_node(
    protocol: &str,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> String {
    debug_assert!(
        serpotter_db::is_allowed_node_protocol(protocol),
        "protocol must be http|https|socks5"
    );
    match (username, password) {
        (Some(u), Some(p)) if !u.is_empty() => {
            format!(
                "{protocol}://{}:{}@{host}:{port}",
                encode_userinfo(u),
                encode_userinfo(p),
            )
        }
        (Some(u), _) if !u.is_empty() => {
            format!("{protocol}://{}@{host}:{port}", encode_userinfo(u))
        }
        _ => format!("{protocol}://{host}:{port}"),
    }
}

fn encode_userinfo(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('@', "%40")
        .replace(':', "%3A")
}

/// Held proxy selection for one attempt (always a real node row).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyLease {
    pub node_id: i64,
    pub url: String,
}

#[derive(Debug, Error)]
pub enum ProxyPoolError {
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Nodes-only outbound side: least-inflight `nodes` lease, or direct when empty.
pub struct ProxyPool {
    db: Db,
    /// Serializes node acquire under SQLite so concurrent picks stay orderly
    /// even if dialect quirks surface; atomic SQL remains the real source of truth.
    lock: Mutex<()>,
    /// When true, product must not dial direct on `acquire` → `None` (fail-closed).
    require_proxy: bool,
    /// Node multi-hold deadline (seconds) stamped on acquire.
    hold_ttl_secs: i64,
}

impl ProxyPool {
    /// Nodes-only pool; `require_proxy` defaults false (empty nodes → direct).
    pub fn new(db: Db) -> Self {
        Self::with_options(db, false)
    }

    /// Hold TTL from `NODE_HOLD_TTL_SECS` (default [`serpotter_db::NODE_HOLD_TTL_SECS`]).
    pub fn with_options(db: Db, require_proxy: bool) -> Self {
        let hold_ttl = std::env::var("NODE_HOLD_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(serpotter_db::NODE_HOLD_TTL_SECS)
            .max(1);
        Self::with_options_and_hold_ttl(db, require_proxy, hold_ttl)
    }

    /// Explicit hold TTL (tests / callers that avoid env).
    pub fn with_options_and_hold_ttl(db: Db, require_proxy: bool, hold_ttl_secs: i64) -> Self {
        Self {
            db,
            lock: Mutex::new(()),
            require_proxy,
            hold_ttl_secs: hold_ttl_secs.max(1),
        }
    }

    /// True when product should refuse direct egress on empty/unhealthy nodes.
    pub fn require_proxy(&self) -> bool {
        self.require_proxy
    }

    /// Atomic least-inflight pick, or `None` when no enabled node.
    pub async fn acquire(&self) -> Result<Option<ProxyLease>, ProxyPoolError> {
        let _guard = self.lock.lock().await;
        match self
            .db
            .acquire_outbound_node_with_ttl(self.hold_ttl_secs)
            .await?
        {
            Some(row) => {
                let url = proxy_url_from_node(
                    &row.protocol,
                    &row.host,
                    row.port as u16,
                    row.username.as_deref(),
                    row.password.as_deref(),
                );
                Ok(Some(ProxyLease {
                    node_id: row.id,
                    url,
                }))
            }
            None => Ok(None),
        }
    }

    /// Success health + inflight--.
    pub async fn report_success(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        self.db.report_node_success(lease.node_id).await?;
        Ok(())
    }

    /// Tunnel-class fail: consecutive_fails++ (disable at 3) + inflight--.
    pub async fn report_failure(
        &self,
        lease: &ProxyLease,
        error: Option<&str>,
    ) -> Result<(), ProxyPoolError> {
        self.db
            .report_node_failure(lease.node_id, serpotter_db::MAX_CONSECUTIVE_FAILURES, error)
            .await?;
        Ok(())
    }

    /// Inflight-- without blaming health.
    pub async fn release(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        self.db.release_node_inflight(lease.node_id).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
