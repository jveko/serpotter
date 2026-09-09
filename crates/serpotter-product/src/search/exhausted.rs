//! Exhausted HTTP status parity (mysearch).

/// Mysearch `EXHAUSTED_STATUS` / `isExhaustedStatus` parity — credit AND plan
/// limits folded into one "exhausted" answer, which is precisely why
/// [`is_payment_required_status`] exists and why the exhausted-vs-payment split
/// is decided by `verdict_for` and both `report_mode` classifiers, each of which
/// tests `402` before consulting this function.
pub fn is_exhausted_status(provider: &str, status: u16) -> bool {
    match provider {
        "tavily" => matches!(status, 429 | 432 | 433),
        "firecrawl" | "exa" => matches!(status, 402 | 429),
        "xai" => status == 429,
        _ => status == 402,
    }
}

/// Upstream `402` Payment Required — the account is out of money, which is a
/// permanent fact, unlike the `429`/`432`/`433` rate-and-plan limits that also
/// count as exhausted per [`is_exhausted_status`].
///
/// The two must be reported to the pool differently: an exhausted key keeps
/// `NULL` credits `NULL` (demoting a healthy account on a rate limit would be
/// wrong), while a `402` key with `NULL` credits can never be demoted by that
/// preserving write — and `NULL` is the normal state for `exa`/`xai`, which are
/// outside the credit-sync allowlist and are seeded without credits. So `402`
/// gets its own mode that zeroes credits unconditionally, otherwise the key
/// keeps its unknown-credit mid-tier score and re-serves `402` forever.
pub fn is_payment_required_status(status: u16) -> bool {
    status == 402
}

#[cfg(test)]
mod payment_required_tests {
    use super::{is_exhausted_status, is_payment_required_status};

    #[test]
    fn only_402_is_payment_required() {
        assert!(is_payment_required_status(402));
        for transient in [429u16, 432, 433, 401, 403, 500, 503] {
            assert!(
                !is_payment_required_status(transient),
                "{transient} must stay on the credit-preserving path"
            );
        }
    }

    /// The conflation this split exists to survive: 402 and 429 are BOTH
    /// "exhausted" for these vendors, so an exhausted-only classifier would
    /// report a rate-limited key as out of money.
    #[test]
    fn exhausted_still_folds_402_and_429() {
        for provider in ["firecrawl", "exa"] {
            assert!(is_exhausted_status(provider, 402));
            assert!(is_exhausted_status(provider, 429));
        }
        assert!(is_exhausted_status("tavily", 429));
        assert!(!is_exhausted_status("tavily", 402));
    }
}

#[cfg(test)]
mod exhausted_tests {
    use super::is_exhausted_status;

    #[test]
    fn tavily_plan_and_paygo() {
        assert!(is_exhausted_status("tavily", 429));
        assert!(is_exhausted_status("tavily", 432));
        assert!(is_exhausted_status("tavily", 433));
        assert!(!is_exhausted_status("tavily", 401));
    }

    #[test]
    fn firecrawl_exa_payment() {
        assert!(is_exhausted_status("firecrawl", 402));
        assert!(is_exhausted_status("exa", 402));
        assert!(is_exhausted_status("exa", 429));
    }

    #[test]
    fn xai_429() {
        assert!(is_exhausted_status("xai", 429));
        assert!(!is_exhausted_status("xai", 402));
    }

    #[test]
    fn unknown_provider_defaults_402() {
        assert!(is_exhausted_status("unknown", 402));
        assert!(!is_exhausted_status("unknown", 429));
    }
}
