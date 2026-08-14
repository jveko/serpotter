//! Outbound HTTP(S)/SOCKS proxy URLs and live node rotation for `reqwest::Proxy::all`.
//!
//! Product path: `ProxyPool::acquire` returns a least-inflight `nodes` lease or `None`
//! (direct). Reqwest owns the CONNECT tunnel via `Proxy::all` — no custom dialer.
//! `proxy_url_from_node` builds scheme URLs from `row.protocol`. When `require_proxy`
//! is set, product maps `None` → `NoHealthyNode` (503). xAI always dials direct.

use std::time::Duration;

use serpotter_db::{Db, DbError, NodeRow};
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
    /// Invalid values are warned about (never silently ignored), then clamped ≥ 1.
    pub fn with_options(db: Db, require_proxy: bool) -> Self {
        let hold_ttl = env_i64_or("NODE_HOLD_TTL_SECS", serpotter_db::NODE_HOLD_TTL_SECS).max(1);
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

    /// Re-stamp the node lease for a still-held lease (long polls — structured
    /// extract — refresh their node lease mid-call so it never expires under
    /// an in-flight hold). Mirrors [`ProxyPool::report_success`]'s shape; a
    /// released/absent node is a no-op success (never an error or panic).
    pub async fn refresh(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        self.db
            .refresh_node_lease(lease.node_id, self.hold_ttl_secs)
            .await?;
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

/// Probe target for [`test_node`]: Google's lightweight `generate_204`
/// endpoint (empty 204, no body, globally reachable). A 2xx through the proxy
/// proves the node can establish a CONNECT tunnel and answer a request end to
/// end — no third-party service dependency to maintain.
const NODE_PROBE_URL: &str = "https://www.gstatic.com/generate_204";

/// Connect-phase budget for a node probe. Explicitly small so a dead node
/// answers in seconds instead of the provider default 10s/60s.
const NODE_PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default total probe budget (connect + full round-trip) used by the admin
/// `POST /api/nodes/{id}/test` handler.
pub const NODE_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Best-effort connectivity probe for one node: build a proxied client from
/// the node row (same `Proxy::all` + explicit-timeout pattern the providers
/// use via `try_build_http`), GET [`NODE_PROBE_URL`] through it, require a
/// 2xx, and return the measured round-trip latency. Errors are classified
/// honestly — transport (refused/timeout/DNS) vs proxy authentication vs a
/// non-2xx upstream status. The success path needs a real, reachable proxy and
/// is intentionally not exercised in CI; the failure path is deterministic.
pub async fn test_node(row: &NodeRow, timeout: Duration) -> Result<Duration, String> {
    let proxy_url = proxy_url_from_node(
        &row.protocol,
        &row.host,
        row.port as u16,
        row.username.as_deref(),
        row.password.as_deref(),
    );
    let proxy = reqwest::Proxy::all(&proxy_url)
        .map_err(|e| format!("invalid proxy URL ({proxy_url}): {e}"))?;
    let client = reqwest::Client::builder()
        .connect_timeout(NODE_PROBE_CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .proxy(proxy)
        .build()
        .map_err(|e| format!("failed to build probe client: {e}"))?;

    let started = std::time::Instant::now();
    let res = client
        .get(NODE_PROBE_URL)
        .send()
        .await
        .map_err(|e| classify_probe_failure(&e))?;
    let status = res.status();
    if status == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED
        || status == reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(format!("proxy authentication failed (HTTP {status})"));
    }
    if !status.is_success() {
        return Err(format!(
            "probe returned HTTP {status} (expected 2xx from {NODE_PROBE_URL})"
        ));
    }
    Ok(started.elapsed())
}

/// Honest transport-vs-auth classification for the probe's reqwest errors.
fn classify_probe_failure(err: &reqwest::Error) -> String {
    let class = if err.is_timeout() {
        "connection timed out"
    } else if err.is_connect() {
        "connection failed"
    } else if err.is_builder() {
        "client build failed"
    } else {
        "request failed"
    };
    // reqwest's top-level Display is a generic "error sending request for
    // url ..." — walk the source chain and surface the innermost cause (e.g.
    // "Connection refused (os error 61)") so the admin UI names the failure.
    let mut innermost: Option<&(dyn std::error::Error + 'static)> = None;
    let mut src = std::error::Error::source(err);
    while let Some(e) = src {
        innermost = Some(e);
        src = e.source();
    }
    match innermost {
        Some(cause) => format!("{class}: {cause}"),
        None => format!("{class}: {err}"),
    }
}

/// Read an integer tuning env var, warning (never silently) when the value is
/// set but unparseable. Missing var → `default` without a warning.
fn env_i64_or(key: &str, default: i64) -> i64 {
    match std::env::var(key) {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    var = key,
                    raw_value = %raw,
                    default,
                    "env value is not a valid integer; using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests;
