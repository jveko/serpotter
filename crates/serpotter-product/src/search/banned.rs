//! Vendor account-ban detection (two tiers).
//!
//! - **High-confidence** ([`is_firecrawl_banned`]): Firecrawl's exact live ban
//!   copy → the key row is hard-DELETEd (irreversible, signature proven).
//! - **Likely** ([`is_likely_banned`]): generic ban/suspension wording on any
//!   other vendor → the key is DISABLED (active=0) instead — instantly out of
//!   rotation but self-healing via `KEY_REENABLE_AFTER_HOURS` if the matcher
//!   over-fired (proxy 403 middleware pages, "plan suspended" quota copy).

/// Live Firecrawl ban body (credit-usage / search / extract), captured 2026-07-30.
#[allow(dead_code)] // shared fixture for Task 2+ on-path delete tests
pub const FIRECRAWL_BAN_BODY_FIXTURE: &str = r#"{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}"#;

const BAN_MARKERS: &[&str] = &["account has been banned", "has been banned"];

/// Generic account-state wording. Deliberately NOT matched for firecrawl
/// (its exact tier runs instead); only strong enough to justify a reversible
/// disable on tavily/exa.
const LIKELY_BAN_MARKERS: &[&str] = &["banned", "suspended", "deactivated", "revoked"];

fn status_gate(status: u16) -> bool {
    status == 401 || status == 403
}

/// True when HTTP status is 401/403 and body matches Firecrawl ban copy.
pub fn is_firecrawl_banned(status: u16, body: &str) -> bool {
    if !status_gate(status) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    BAN_MARKERS.iter().any(|m| lower.contains(m))
}

/// True when HTTP status is 401/403 and body carries generic account-ban /
/// suspension wording. Softer signal than [`is_firecrawl_banned`] — callers
/// must pair it with a reversible disable, never a delete.
pub fn is_likely_banned(status: u16, body: &str) -> bool {
    if !status_gate(status) {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    LIKELY_BAN_MARKERS.iter().any(|m| lower.contains(m))
}

/// Provider-dispatched ban check: firecrawl uses its proven high-confidence
/// signature; every other vendor uses the likely-tier matcher.
pub fn is_account_banned(provider: &str, status: u16, body: &str) -> bool {
    if provider == "firecrawl" {
        is_firecrawl_banned(status, body)
    } else {
        is_likely_banned(status, body)
    }
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
        assert!(is_firecrawl_banned(
            403,
            "sorry, has been banned permanently"
        ));
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

    #[test]
    fn likely_tier_matches_generic_wording() {
        assert!(is_likely_banned(403, r#"{"error":"plan suspended"}"#));
        assert!(is_likely_banned(401, "account deactivated by admin"));
        assert!(is_likely_banned(403, "API key revoked"));
    }

    #[test]
    fn likely_tier_ignores_plain_auth_and_quota_copy() {
        assert!(!is_likely_banned(403, r#"{"error":"Unauthorized"}"#));
        assert!(!is_likely_banned(429, "rate limited: plan suspended soon"));
        assert!(!is_likely_banned(402, "payment required"));
    }

    #[test]
    fn dispatcher_routes_by_provider() {
        // firecrawl → exact tier only
        assert!(is_account_banned(
            "firecrawl",
            403,
            FIRECRAWL_BAN_BODY_FIXTURE
        ));
        assert!(!is_account_banned(
            "firecrawl",
            403,
            r#"{"error":"key revoked"}"#
        ));
        // tavily/exa/xai → likely tier
        assert!(is_account_banned(
            "tavily",
            403,
            r#"{"error":"account suspended"}"#
        ));
        assert!(is_account_banned(
            "exa",
            401,
            r#"{"detail":"user deactivated"}"#
        ));
        assert!(!is_account_banned(
            "tavily",
            403,
            r#"{"error":"Unauthorized"}"#
        ));
    }
}
