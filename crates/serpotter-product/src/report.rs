//! Dual-pool outcome helpers (no I/O).

/// How to report key + proxy when a `ProviderError::Http` occurs.
///
/// With a proxy lease, **decode / body / non-tunnel** HTTP errors must not burn
/// node health (false fail@3 after 2xx + bad JSON). Tunnel/connect class burns
/// the node; keys stay release-only for any proxied transport path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxiedHttpClass {
    /// No proxy lease: key failure (consecutive_fails++).
    DirectKeyFailure,
    /// Proxy present + tunnel/connect class: key release + node failure.
    TunnelKeyReleaseNodeFailure,
    /// Proxy present + non-tunnel (e.g. JSON decode after 2xx): both release-only.
    BothReleaseOnly,
}

/// Classify a transport error for the dual-pool matrix.
///
/// `had_proxy_lease` — product held a proxy lease for this attempt (including Fixed).
/// `tunnel` — `serpotter_providers::is_tunnel_error` (or equivalent) for the err.
pub fn classify_proxied_http(had_proxy_lease: bool, tunnel: bool) -> ProxiedHttpClass {
    if !had_proxy_lease {
        return ProxiedHttpClass::DirectKeyFailure;
    }
    if tunnel {
        ProxiedHttpClass::TunnelKeyReleaseNodeFailure
    } else {
        ProxiedHttpClass::BothReleaseOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use serpotter_db::connect_and_migrate;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{is_tunnel_error, try_build_http, ProviderError};
    use std::time::Duration;

    #[test]
    fn direct_http_is_key_failure() {
        assert_eq!(
            classify_proxied_http(false, true),
            ProxiedHttpClass::DirectKeyFailure
        );
        assert_eq!(
            classify_proxied_http(false, false),
            ProxiedHttpClass::DirectKeyFailure
        );
    }

    #[test]
    fn proxy_tunnel_vs_decode() {
        assert_eq!(
            classify_proxied_http(true, true),
            ProxiedHttpClass::TunnelKeyReleaseNodeFailure
        );
        assert_eq!(
            classify_proxied_http(true, false),
            ProxiedHttpClass::BothReleaseOnly
        );
    }

    #[test]
    fn bad_proxy_build_is_tunnel_class() {
        let err = match try_build_http(Some("not a proxy url :::")) {
            Err(ProviderError::Http(e)) => e,
            other => panic!("expected Http build err, got {other:?}"),
        };
        assert!(
            is_tunnel_error(&err),
            "builder/proxy parse should classify as tunnel: {err}"
        );
        assert_eq!(
            classify_proxied_http(true, is_tunnel_error(&err)),
            ProxiedHttpClass::TunnelKeyReleaseNodeFailure
        );
    }

    /// Product dual-matrix: tunnel-class with a node lease must not bump key fails.
    #[tokio::test]
    async fn matrix_tunnel_does_not_increment_key_fails() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let k = db.insert_api_key("tavily", "tvly-m").await.unwrap();
        let n = db
            .insert_node("proxy.example", 8080, None, None)
            .await
            .unwrap();
        let keys = Arc::new(KeyPool::with_config(
            db.clone(),
            3,
            Duration::from_secs(5),
            90,
        ));
        let outbound = Arc::new(ProxyPool::from_env_and_db(None, db.clone()));

        let lease_k = keys.acquire("tavily").await.unwrap();
        let lease_p = outbound.acquire().await.unwrap().expect("node");
        assert_eq!(lease_p.node_id, Some(n.id));

        assert_eq!(
            classify_proxied_http(true, true),
            ProxiedHttpClass::TunnelKeyReleaseNodeFailure
        );
        keys.release(lease_k.id).await.unwrap();
        outbound.report_failure(&lease_p, None).await.unwrap();

        let key_row = db.get_api_key(k.id).await.unwrap().unwrap();
        assert_eq!(
            key_row.consecutive_fails, 0,
            "tunnel must not fail@ key"
        );
        let node = db
            .list_nodes()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == n.id)
            .expect("node");
        assert_eq!(node.consecutive_fails, 1, "tunnel blames node");
    }

    /// Non-tunnel Http with proxy (decode-class): both release-only — no node fail++.
    #[tokio::test]
    async fn matrix_decode_does_not_increment_node_fails() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let k = db.insert_api_key("tavily", "tvly-d").await.unwrap();
        let n = db
            .insert_node("proxy2.example", 8080, None, None)
            .await
            .unwrap();
        let keys = Arc::new(KeyPool::with_config(
            db.clone(),
            3,
            Duration::from_secs(5),
            90,
        ));
        let outbound = Arc::new(ProxyPool::from_env_and_db(None, db.clone()));

        let lease_k = keys.acquire("tavily").await.unwrap();
        let lease_p = outbound.acquire().await.unwrap().expect("node");

        assert_eq!(
            classify_proxied_http(true, false),
            ProxiedHttpClass::BothReleaseOnly
        );
        keys.release(lease_k.id).await.unwrap();
        outbound.release(&lease_p).await.unwrap();

        let key_row = db.get_api_key(k.id).await.unwrap().unwrap();
        assert_eq!(key_row.consecutive_fails, 0);
        let node = db
            .list_nodes()
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == n.id)
            .expect("node");
        assert_eq!(node.consecutive_fails, 0, "decode-class must not fail@ node");
        let _ = k;
    }
}
