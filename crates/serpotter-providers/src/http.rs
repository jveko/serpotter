//! Shared reqwest client construction and proxy client cache.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use reqwest::Client;

use crate::ProviderError;

pub const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Soft cap on distinct proxied clients; drop an arbitrary entry when exceeded.
const CACHE_SOFT_MAX: usize = 32;

/// Build a reqwest client with connect/request timeouts.
///
/// - `None` → direct client (no proxy).
/// - `Some(url)` → attaches `Proxy::all`; **errors** if the proxy URL fails to parse
///   or the client fails to build (no silent direct fallback).
pub fn try_build_http(proxy_url: Option<&str>) -> Result<Client, ProviderError> {
    let mut b = Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT);
    if let Some(p) = proxy_url {
        let proxy = reqwest::Proxy::all(p).map_err(ProviderError::Http)?;
        b = b.proxy(proxy);
    }
    b.build().map_err(ProviderError::Http)
}

/// Direct (non-proxied) client. Panics only if the default builder is broken.
pub fn build_direct() -> Client {
    try_build_http(None).expect("reqwest direct client")
}

/// Classify reqwest errors that are likely proxy/tunnel/connectivity failures
/// (vs HTTP status upstream errors). Used by product dual-pool matrix.
///
/// Returns `true` ONLY for **connection-establishment** failures — DNS failure,
/// connection refused/reset, or a connect-phase timeout (`err.is_connect()`).
/// Request-phase timeouts (`err.is_timeout()` but the connect already
/// succeeded) are **upstream stalls**, not tunnel errors: the node (or direct
/// path) is healthy, the upstream just did not answer in time. Failing the
/// node on those would disable healthy proxies for a slow vendor.
pub fn is_tunnel_error(err: &reqwest::Error) -> bool {
    if err.is_connect() {
        return true;
    }
    // Proxy handshake / CONNECT / client-builder failures (hard-fail Proxy::all).
    if err.is_request() || err.is_builder() {
        let s = err.to_string().to_ascii_lowercase();
        if s.contains("proxy")
            || s.contains("tunnel")
            || s.contains("connect")
            || s.contains("builder")
        {
            return true;
        }
    }
    // Source chain: hyper/reqwest may nest connect errors. Deliberately do NOT
    // match bare "timeout"/"timed out" strings here: a request-phase stall after
    // a successful connect ("operation timed out") must not classify as tunnel.
    let mut src = std::error::Error::source(err);
    while let Some(e) = src {
        let s = e.to_string().to_ascii_lowercase();
        if s.contains("proxy")
            || s.contains("tunnel")
            || s.contains("connection refused")
            || s.contains("connection reset")
            || s.contains("connect")
            || s.contains("builder")
        {
            return true;
        }
        src = e.source();
    }
    false
}

/// Cached proxied clients + one shared direct client.
///
/// Clones share the same `Arc` map (cheap `Clone` for `ProviderRegistry`).
#[derive(Clone)]
pub struct ClientCache {
    direct: Client,
    cache: Arc<Mutex<HashMap<String, Client>>>,
}

impl ClientCache {
    pub fn new() -> Self {
        Self {
            direct: build_direct(),
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn direct(&self) -> Client {
        self.direct.clone()
    }

    /// Resolve a client for this proxy URL (or direct when `None`).
    /// Caches successful proxied builds keyed by exact URL string.
    pub fn client_for(&self, proxy: Option<&str>) -> Result<Client, ProviderError> {
        match proxy {
            None => Ok(self.direct.clone()),
            Some(url) => {
                let mut g = self.cache.lock();
                if let Some(c) = g.get(url) {
                    return Ok(c.clone());
                }
                let c = try_build_http(Some(url))?;
                if g.len() >= CACHE_SOFT_MAX {
                    // Drop an arbitrary entry (HashMap iteration order is unspecified).
                    if let Some(k) = g.keys().next().cloned() {
                        g.remove(&k);
                    }
                }
                g.insert(url.to_string(), c.clone());
                Ok(c)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.lock().len()
    }
}

impl Default for ClientCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_build_http_none_ok() {
        let c = try_build_http(None).expect("direct");
        // Client is usable (clone is cheap Arc share).
        let _ = c.clone();
    }

    #[test]
    fn try_build_http_bad_proxy_is_err() {
        let err = try_build_http(Some("not-a-url-:::")).expect_err("must hard-fail");
        match err {
            ProviderError::Http(_) => {}
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn try_build_http_valid_proxy_url_ok() {
        // Parse/build only — no network.
        try_build_http(Some("http://proxy.example:8080")).expect("valid proxy url");
    }

    #[test]
    fn client_for_caches_same_url() {
        let cache = ClientCache::new();
        let a = cache
            .client_for(Some("http://proxy.example:8080"))
            .expect("build");
        let b = cache
            .client_for(Some("http://proxy.example:8080"))
            .expect("cached");
        assert_eq!(cache.cache_len(), 1);
        // reqwest::Client clones share the same inner Arc — pointer equality on debug?
        // Compare via as_ref identity through type-erased: both work for requests.
        let _ = (a, b);
        let _ = cache
            .client_for(Some("http://other.example:1"))
            .expect("second");
        assert_eq!(cache.cache_len(), 2);
    }

    #[test]
    fn client_for_none_is_direct_no_cache_entry() {
        let cache = ClientCache::new();
        let _ = cache.client_for(None).expect("direct");
        assert_eq!(cache.cache_len(), 0);
    }

    #[test]
    fn client_for_bad_proxy_err() {
        let cache = ClientCache::new();
        assert!(cache.client_for(Some("not-a-url-:::")).is_err());
        assert_eq!(cache.cache_len(), 0);
    }

    /// Connection refused is a connect-class failure → tunnel (node blamed).
    #[tokio::test]
    async fn connect_refused_is_tunnel() {
        let client = reqwest::Client::builder().build().expect("client");
        let err = client
            .get("http://127.0.0.1:9/")
            .send()
            .await
            .expect_err("connection refused on :9");
        assert!(err.is_connect(), "refused must be connect-class: {err}");
        assert!(
            is_tunnel_error(&err),
            "connect-class failure must be a tunnel error: {err}"
        );
    }

    /// A request-phase timeout (connect succeeded, upstream stalled) must NOT be
    /// a tunnel error: the node is healthy, the vendor is slow. Regression for
    /// the bug where any `is_timeout()` error disabled healthy proxy nodes.
    #[tokio::test]
    async fn request_phase_timeout_is_not_tunnel() {
        // Accept the connection, then stall without responding. The client's
        // request timeout fires AFTER the connect phase → upstream stall.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            std::thread::sleep(std::time::Duration::from_secs(5));
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_millis(300))
            .build()
            .expect("client");
        let err = client
            .get(format!("http://{addr}/stall"))
            .send()
            .await
            .expect_err("request-phase timeout");
        assert!(err.is_timeout(), "expected a timeout: {err}");
        assert!(
            !err.is_connect(),
            "connect succeeded; the stall is upstream-side: {err}"
        );
        assert!(
            !is_tunnel_error(&err),
            "request-phase timeout must not be a tunnel error: {err}"
        );
    }
}
