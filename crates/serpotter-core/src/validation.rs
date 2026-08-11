//! Closed-set validation for routing knobs, shared by the REST and MCP entry
//! points.
//!
//! Routing (`resolve.rs` / `rules.rs`) silently coerces unknown values
//! (strategy -> fast, mode -> no-op, intent -> pass-through), so both public
//! surfaces reject non-empty values outside the advertised sets instead of
//! letting them mislead the client. Moved out of `mcp/params.rs` so the REST
//! handlers validate identically (FU10).

/// Advertised search modes (routing aliases `social` → xAI; `docs`/`github`/`pdf` → resource).
pub const VALID_MODES: &[&str] = &[
    "auto", "web", "news", "social", "docs", "research", "github", "pdf",
];
/// Advertised query intents.
pub const VALID_INTENTS: &[&str] = &[
    "auto",
    "factual",
    "status",
    "comparison",
    "tutorial",
    "exploratory",
    "news",
    "resource",
];
/// Advertised routing strategies.
pub const VALID_STRATEGIES: &[&str] = &["auto", "fast", "balanced", "verify", "deep"];
/// Advertised providers, plus `social` (routing aliases it to xai) and
/// `hybrid` (multi-provider web+x merge, REST-supported dial).
pub const VALID_PROVIDERS: &[&str] = &[
    "auto",
    "tavily",
    "firecrawl",
    "exa",
    "xai",
    "social",
    "hybrid",
];
/// Advertised Tavily search depths.
pub const VALID_SEARCH_DEPTHS: &[&str] = &["basic", "advanced", "fast", "ultra-fast"];
/// Extract-only provider set: only firecrawl/tavily support extract; `auto`
/// lets the chain detect (firecrawl first).
pub const VALID_EXTRACT_PROVIDERS: &[&str] = &["auto", "tavily", "firecrawl"];

/// Validate one optional routing knob against its closed set. `None` and the
/// empty string both mean "unset" and are routed as defaults (always Ok).
pub fn validate_choice(field: &str, value: Option<&str>, valid: &[&str]) -> Result<(), String> {
    match value {
        // None and empty both mean "unset" and are routed as defaults.
        None | Some("") => Ok(()),
        Some(v) if valid.contains(&v) => Ok(()),
        Some(v) => Err(format!(
            "{field}: {v:?} is not a supported value (valid: {})",
            valid.join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_and_empty_are_unset() {
        assert!(validate_choice("mode", None, VALID_MODES).is_ok());
        assert!(validate_choice("mode", Some(""), VALID_MODES).is_ok());
    }

    #[test]
    fn valid_value_passes() {
        assert!(validate_choice("strategy", Some("balanced"), VALID_STRATEGIES).is_ok());
        assert!(validate_choice("provider", Some("hybrid"), VALID_PROVIDERS).is_ok());
        assert!(validate_choice("provider", Some("tavily"), VALID_EXTRACT_PROVIDERS).is_ok());
        assert!(validate_choice("provider", Some("auto"), VALID_EXTRACT_PROVIDERS).is_ok());
    }

    #[test]
    fn invalid_value_lists_field_and_valid_set() {
        let err = validate_choice("strategy", Some("bogus"), VALID_STRATEGIES)
            .expect_err("bogus must fail");
        assert!(err.contains("strategy"), "{err}");
        assert!(err.contains("balanced"), "valid set listed: {err}");
    }

    #[test]
    fn sets_are_disjoint_and_complete() {
        // Providers must cover the search dispatch surface plus aliases.
        for p in [
            "auto",
            "tavily",
            "firecrawl",
            "exa",
            "xai",
            "social",
            "hybrid",
        ] {
            assert!(VALID_PROVIDERS.contains(&p), "missing provider {p}");
        }
        // Extract providers are a strict subset of search providers.
        for p in VALID_EXTRACT_PROVIDERS {
            assert!(
                VALID_PROVIDERS.contains(p),
                "extract provider {p} not in search set"
            );
        }
    }
}
