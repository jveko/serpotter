//! Outbound HTTP(S) proxy URLs and live node rotation for `reqwest::Proxy::all`.
//!
//! Product path: `ProxyPool::acquire` returns Fixed env URL, a least-inflight
//! `nodes` lease, or `None` (direct). Reqwest owns the CONNECT tunnel via
//! `Proxy::all` — no custom dialer. `proxy_url_from_node` builds node URLs.
//! When `require_proxy` is set, product maps `None` → `NoHealthyNode` (503).

use serpotter_db::{Db, DbError};
use thiserror::Error;
use tokio::sync::Mutex;

/// Build `http://[user:pass@]host:port` for `reqwest::Proxy::all`.
pub fn proxy_url_from_node(
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> String {
    match (username, password) {
        (Some(u), Some(p)) if !u.is_empty() => {
            format!(
                "http://{}:{}@{}:{}",
                encode_userinfo(u),
                encode_userinfo(p),
                host,
                port
            )
        }
        (Some(u), _) if !u.is_empty() => {
            format!("http://{}@{}:{}", encode_userinfo(u), host, port)
        }
        _ => format!("http://{host}:{port}"),
    }
}

fn encode_userinfo(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('@', "%40")
        .replace(':', "%3A")
}

/// Held proxy selection for one attempt. `node_id = None` for Fixed env leases.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyLease {
    pub node_id: Option<i64>,
    pub url: String,
}

#[derive(Debug, Error)]
pub enum ProxyPoolError {
    #[error(transparent)]
    Db(#[from] DbError),
}

enum Mode {
    /// Process-stable env proxy; never touch `nodes`.
    Fixed(String),
    /// Live least-inflight pick from `nodes` (or direct when empty).
    Nodes(Db),
}

/// Twin-pool outbound side: Fixed env | live nodes | direct.
pub struct ProxyPool {
    mode: Mode,
    /// Serializes node acquire under SQLite so concurrent picks stay orderly
    /// even if dialect quirks surface; atomic SQL remains the real source of truth.
    lock: Mutex<()>,
    /// When true, product must not dial direct on `acquire` → `None` (fail-closed).
    require_proxy: bool,
    /// Node multi-hold deadline (seconds) stamped on acquire.
    hold_ttl_secs: i64,
}

impl ProxyPool {
    /// Decision tree is fixed at construction from `env_proxy`:
    /// non-empty → Fixed forever; else Nodes mode over `db`.
    /// `require_proxy` defaults false (empty nodes → direct).
    /// Hold TTL from `NODE_HOLD_TTL_SECS` (default [`serpotter_db::NODE_HOLD_TTL_SECS`]).
    pub fn from_env_and_db(env_proxy: Option<String>, db: Db) -> Self {
        Self::with_options(env_proxy, db, false)
    }

    /// Same as [`from_env_and_db`] with explicit fail-closed flag.
    pub fn with_options(env_proxy: Option<String>, db: Db, require_proxy: bool) -> Self {
        let hold_ttl = std::env::var("NODE_HOLD_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(serpotter_db::NODE_HOLD_TTL_SECS)
            .max(1);
        Self::with_options_and_hold_ttl(env_proxy, db, require_proxy, hold_ttl)
    }

    /// Explicit hold TTL (tests / callers that avoid env).
    pub fn with_options_and_hold_ttl(
        env_proxy: Option<String>,
        db: Db,
        require_proxy: bool,
        hold_ttl_secs: i64,
    ) -> Self {
        let fixed = env_proxy
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Self {
            mode: match fixed {
                Some(url) => {
                    drop(db); // Fixed: ignore nodes for process lifetime
                    Mode::Fixed(url)
                }
                None => Mode::Nodes(db),
            },
            lock: Mutex::new(()),
            require_proxy,
            hold_ttl_secs: hold_ttl_secs.max(1),
        }
    }

    /// True when product should refuse direct egress on empty/unhealthy nodes.
    pub fn require_proxy(&self) -> bool {
        self.require_proxy
    }

    /// Fixed → always same lease (`node_id = None`).
    /// Nodes → atomic least-inflight pick, or `None` when no enabled node.
    pub async fn acquire(&self) -> Result<Option<ProxyLease>, ProxyPoolError> {
        match &self.mode {
            Mode::Fixed(url) => Ok(Some(ProxyLease {
                node_id: None,
                url: url.clone(),
            })),
            Mode::Nodes(db) => {
                let _guard = self.lock.lock().await;
                match db
                    .acquire_outbound_node_with_ttl(self.hold_ttl_secs)
                    .await?
                {
                    Some(row) => {
                        let url = proxy_url_from_node(
                            &row.host,
                            row.port as u16,
                            row.username.as_deref(),
                            row.password.as_deref(),
                        );
                        Ok(Some(ProxyLease {
                            node_id: Some(row.id),
                            url,
                        }))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    /// Success health + inflight--. Fixed / `node_id = None` → no DB.
    pub async fn report_success(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        let Some(id) = lease.node_id else {
            return Ok(());
        };
        let Some(db) = self.db() else {
            return Ok(());
        };
        db.report_node_success(id).await?;
        Ok(())
    }

    /// Tunnel-class fail: consecutive_fails++ (disable at 3) + inflight--.
    /// Fixed / `node_id = None` → no DB.
    pub async fn report_failure(
        &self,
        lease: &ProxyLease,
        error: Option<&str>,
    ) -> Result<(), ProxyPoolError> {
        let Some(id) = lease.node_id else {
            return Ok(());
        };
        let Some(db) = self.db() else {
            return Ok(());
        };
        db.report_node_failure(id, serpotter_db::MAX_CONSECUTIVE_FAILURES, error)
            .await?;
        Ok(())
    }

    /// Inflight-- without blaming health. Fixed / `node_id = None` → no DB.
    pub async fn release(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        let Some(id) = lease.node_id else {
            return Ok(());
        };
        let Some(db) = self.db() else {
            return Ok(());
        };
        db.release_node_inflight(id).await?;
        Ok(())
    }

    fn db(&self) -> Option<&Db> {
        match &self.mode {
            Mode::Fixed(_) => None,
            Mode::Nodes(db) => Some(db),
        }
    }
}


#[cfg(test)]
mod tests;
