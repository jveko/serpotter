//! Exhausted HTTP status parity (mysearch).

/// Mysearch `EXHAUSTED_STATUS` / `isExhaustedStatus` parity.
/// Credit/plan limits → `report_exhausted` (not consecutive fail).
pub fn is_exhausted_status(provider: &str, status: u16) -> bool {
    match provider {
        "tavily" => matches!(status, 429 | 432 | 433),
        "firecrawl" | "exa" => matches!(status, 402 | 429),
        "xai" => status == 429,
        _ => status == 402,
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
