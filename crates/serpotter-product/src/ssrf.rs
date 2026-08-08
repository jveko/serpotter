//! Lightweight extract URL gate: https/http + public hosts only.

use std::net::IpAddr;

use url::Url;

use crate::error::ExtractError;

/// Reject non-http(s), missing host, credentials, and private/loopback/link-local hosts.
pub fn validate_extract_url(raw: &str) -> Result<String, ExtractError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ExtractError::InvalidUrl("empty url".into()));
    }
    let url =
        Url::parse(trimmed).map_err(|e| ExtractError::InvalidUrl(format!("invalid url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(ExtractError::InvalidUrl(format!(
                "unsupported scheme {other} (http/https only)"
            )));
        }
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ExtractError::InvalidUrl(
            "url must not include credentials".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| ExtractError::InvalidUrl("missing host".into()))?;
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return Err(ExtractError::InvalidUrl("localhost not allowed".into()));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_non_public_ip(ip) {
            return Err(ExtractError::InvalidUrl(format!(
                "non-public host not allowed: {host}"
            )));
        }
    } else if looks_like_numeric_ip(host) {
        // Unparseable "IP-like" host — reject rather than allow.
        return Err(ExtractError::InvalidUrl(format!(
            "non-public host not allowed: {host}"
        )));
    }
    Ok(trimmed.to_string())
}

fn looks_like_numeric_ip(host: &str) -> bool {
    // IPv4 dotted: all labels digits
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    host.contains(':')
}

fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                // CGNAT 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
                // 192.0.0.0/24 (IETF protocol assignments)
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || v6.to_ipv4_mapped().is_some_and(|v4| {
                    v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_public_https() {
        assert!(validate_extract_url("https://example.com/path").is_ok());
        assert!(validate_extract_url("http://docs.rs").is_ok());
    }

    #[test]
    fn rejects_localhost_and_loopback() {
        assert!(validate_extract_url("http://localhost/x").is_err());
        assert!(validate_extract_url("http://127.0.0.1/x").is_err());
        assert!(validate_extract_url("http://[::1]/x").is_err());
        assert!(validate_extract_url("http://foo.localhost/").is_err());
    }

    #[test]
    fn rejects_private_and_file() {
        assert!(validate_extract_url("http://10.0.0.1/").is_err());
        assert!(validate_extract_url("http://192.168.1.1/").is_err());
        assert!(validate_extract_url("http://172.16.0.1/").is_err());
        assert!(validate_extract_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn rejects_credentials() {
        assert!(validate_extract_url("https://user:pass@example.com/").is_err());
    }
}
