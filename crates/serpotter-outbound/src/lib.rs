//! Outbound proxy URLs for `reqwest::Proxy::all`.
//!
//! Product path: resolve one URL (env preferred, else optional DB node) and pass it to
//! `ProviderRegistry::with_proxy_url`. No custom CONNECT dialer — reqwest owns the tunnel.

/// Prefer explicit env proxy, else optional DB-derived URL.
pub fn resolve_outbound_proxy_url(
    env_proxy: Option<String>,
    node_proxy: Option<String>,
) -> Option<String> {
    env_proxy
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            node_proxy
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_beats_node() {
        assert_eq!(
            resolve_outbound_proxy_url(
                Some("http://env-proxy:1".into()),
                Some("http://node:2".into())
            )
            .as_deref(),
            Some("http://env-proxy:1")
        );
    }

    #[test]
    fn node_when_no_env() {
        assert_eq!(
            resolve_outbound_proxy_url(None, Some("http://node:8080".into())).as_deref(),
            Some("http://node:8080")
        );
    }

    #[test]
    fn empty_env_falls_to_node() {
        assert_eq!(
            resolve_outbound_proxy_url(Some("  ".into()), Some("http://n:1".into())).as_deref(),
            Some("http://n:1")
        );
    }

    #[test]
    fn proxy_url_with_auth() {
        assert_eq!(
            proxy_url_from_node("proxy.example", 8080, Some("u"), Some("p")),
            "http://u:p@proxy.example:8080"
        );
    }
}
