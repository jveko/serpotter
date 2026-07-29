//! Firecrawl permanent-ban body detection (on-path key delete).

/// Live Firecrawl ban body (credit-usage / search / extract), captured 2026-07-30.
#[allow(dead_code)] // shared fixture for Task 2+ on-path delete tests
pub const FIRECRAWL_BAN_BODY_FIXTURE: &str = r#"{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}"#;

const BAN_MARKERS: &[&str] = &["account has been banned", "has been banned"];

/// True when HTTP status is 401/403 and body matches Firecrawl ban copy.
/// Caller must only use this for `provider == "firecrawl"`.
pub fn is_firecrawl_banned(status: u16, body: &str) -> bool {
    if status != 401 && status != 403 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    BAN_MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod banned_tests {
    use super::*;

    #[test]
    fn fixture_403_is_banned() {
        assert!(is_firecrawl_banned(403, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn fixture_401_is_banned() {
        assert!(is_firecrawl_banned(401, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn case_insensitive() {
        assert!(is_firecrawl_banned(
            403,
            r#"{"error":"ACCOUNT HAS BEEN BANNED by ops"}"#
        ));
    }

    #[test]
    fn short_marker_has_been_banned() {
        assert!(is_firecrawl_banned(403, "sorry, has been banned permanently"));
    }

    #[test]
    fn plain_403_unauthorized_not_banned() {
        assert!(!is_firecrawl_banned(
            403,
            r#"{"success":false,"error":"Unauthorized"}"#
        ));
    }

    #[test]
    fn status_402_not_banned_even_with_marker() {
        assert!(!is_firecrawl_banned(402, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn status_429_500_not_banned() {
        assert!(!is_firecrawl_banned(429, FIRECRAWL_BAN_BODY_FIXTURE));
        assert!(!is_firecrawl_banned(500, FIRECRAWL_BAN_BODY_FIXTURE));
    }
}
