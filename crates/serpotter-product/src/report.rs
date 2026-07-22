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
    use serpotter_providers::{is_tunnel_error, try_build_http, ProviderError};

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
}
