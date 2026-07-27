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
}

impl ProxyPool {
    /// Decision tree is fixed at construction from `env_proxy`:
    /// non-empty → Fixed forever; else Nodes mode over `db`.
    /// `require_proxy` defaults false (empty nodes → direct).
    pub fn from_env_and_db(env_proxy: Option<String>, db: Db) -> Self {
        Self::with_options(env_proxy, db, false)
    }

    /// Same as [`from_env_and_db`] with explicit fail-closed flag.
    pub fn with_options(env_proxy: Option<String>, db: Db, require_proxy: bool) -> Self {
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
                match db.acquire_outbound_node().await? {
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
mod tests {
    use super::*;
    use serpotter_db::connect_and_migrate;
    use std::sync::Arc;

    #[test]
    fn proxy_url_with_auth() {
        assert_eq!(
            proxy_url_from_node("proxy.example", 8080, Some("u"), Some("p")),
            "http://u:p@proxy.example:8080"
        );
    }

    #[tokio::test]
    async fn fixed_mode_ignores_nodes() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let node = db
            .insert_node("node.example", 9000, None, None)
            .await
            .unwrap();
        let pool = ProxyPool::from_env_and_db(Some("http://fixed.proxy:3128".into()), db.clone());

        let lease = pool.acquire().await.unwrap().expect("fixed always Some");
        assert_eq!(lease.node_id, None);
        assert_eq!(lease.url, "http://fixed.proxy:3128");

        // Second acquire still fixed URL — never the node.
        let again = pool.acquire().await.unwrap().unwrap();
        assert_eq!(again.node_id, None);
        assert_eq!(again.url, "http://fixed.proxy:3128");

        let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
        assert_eq!(row.id, node.id);
        assert_eq!(row.inflight, 0, "fixed must not bump node inflight");
    }

    #[tokio::test]
    async fn empty_nodes_returns_none_direct() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let pool = ProxyPool::from_env_and_db(None, db);
        assert!(pool.acquire().await.unwrap().is_none());
        assert!(!pool.require_proxy());
    }

    #[tokio::test]
    async fn require_proxy_flag_preserved_on_empty_nodes() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let pool = ProxyPool::with_options(None, db, true);
        assert!(pool.require_proxy());
        assert!(pool.acquire().await.unwrap().is_none());
    }
    #[tokio::test]
    async fn release_decrements_inflight() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let n = db
            .insert_node("rel.example", 8080, None, None)
            .await
            .unwrap();
        let pool = ProxyPool::from_env_and_db(None, db.clone());

        let lease = pool.acquire().await.unwrap().unwrap();
        assert_eq!(lease.node_id, Some(n.id));
        assert_eq!(
            db.list_nodes().await.unwrap()[0].inflight,
            1,
            "acquire bumps inflight"
        );

        pool.release(&lease).await.unwrap();
        assert_eq!(
            db.list_nodes().await.unwrap()[0].inflight,
            0,
            "release must decrement"
        );
    }

    #[tokio::test]
    async fn report_failure_disables_at_three() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let n = db
            .insert_node("fail.example", 8080, None, None)
            .await
            .unwrap();
        let pool = ProxyPool::from_env_and_db(None, db.clone());

        for i in 1..=3 {
            let lease = pool.acquire().await.unwrap().expect("node still enabled");
            assert_eq!(lease.node_id, Some(n.id));
            pool.report_failure(&lease, None).await.unwrap();
            let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
            assert_eq!(row.consecutive_fails, i);
            if i < 3 {
                assert_eq!(row.enabled, 1);
            } else {
                assert_eq!(row.enabled, 0);
                assert_eq!(row.inflight, 0);
            }
        }

        assert!(
            pool.acquire().await.unwrap().is_none(),
            "disabled node → direct"
        );
    }

    #[tokio::test]
    async fn fixed_report_is_noop_on_nodes() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let n = db
            .insert_node("noop.example", 8080, None, None)
            .await
            .unwrap();
        // Seed inflight so we can detect accidental release/report.
        db.bump_node_inflight(n.id, 2).await.unwrap();
        let pool = ProxyPool::from_env_and_db(Some("http://fixed:1".into()), db.clone());

        let lease = pool.acquire().await.unwrap().unwrap();
        assert_eq!(lease.node_id, None);

        pool.report_success(&lease).await.unwrap();
        pool.report_failure(&lease, None).await.unwrap();
        pool.release(&lease).await.unwrap();

        let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
        assert_eq!(row.id, n.id);
        assert_eq!(row.inflight, 2, "fixed reports must not touch nodes SQL");
        assert_eq!(row.consecutive_fails, 0);
        assert_eq!(row.enabled, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_acquire_least_inflight_distinct() {
        // File DB allows multi-connection; :memory: pool is max_connections=1.
        let path = std::env::temp_dir().join(format!(
            "serpotter-outbound-pool-{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let db = connect_and_migrate(&url).await.unwrap();
        let a = db.insert_node("a.example", 8001, None, None).await.unwrap();
        let b = db.insert_node("b.example", 8002, None, None).await.unwrap();
        let pool = Arc::new(ProxyPool::from_env_and_db(None, db.clone()));

        let p1 = Arc::clone(&pool);
        let p2 = Arc::clone(&pool);
        let (r1, r2) = tokio::join!(p1.acquire(), p2.acquire());
        let l1 = r1.unwrap().expect("lease1");
        let l2 = r2.unwrap().expect("lease2");

        let ids: std::collections::HashSet<i64> = [l1.node_id.unwrap(), l2.node_id.unwrap()]
            .into_iter()
            .collect();
        assert_eq!(
            ids.len(),
            2,
            "two concurrent acquires on tied inflight must pick distinct nodes"
        );
        assert!(ids.contains(&a.id) && ids.contains(&b.id));

        pool.release(&l1).await.unwrap();
        pool.release(&l2).await.unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn nodes_mode_builds_url_from_row() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        db.insert_node("proxy.example", 8080, Some("u"), Some("p"))
            .await
            .unwrap();
        let pool = ProxyPool::from_env_and_db(None, db);
        let lease = pool.acquire().await.unwrap().unwrap();
        assert_eq!(lease.url, "http://u:p@proxy.example:8080");
        pool.report_success(&lease).await.unwrap();
    }

    #[tokio::test]
    async fn whitespace_env_is_not_fixed() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        db.insert_node("ws.example", 1, None, None).await.unwrap();
        let pool = ProxyPool::from_env_and_db(Some("   ".into()), db);
        let lease = pool.acquire().await.unwrap().unwrap();
        assert!(lease.node_id.is_some(), "blank env must fall through to nodes");
    }
}
