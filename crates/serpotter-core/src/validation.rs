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
/// Advertised search sources. `web`/`x` keep their routing semantics
/// (web web-leg / social x-leg); `social` is an alias for `x`; `news`
/// routes to Tavily news topic; `images` routes to Firecrawl image search
/// (categories). Unknown sources are client errors, never silent no-ops.
pub const VALID_SOURCES: &[&str] = &["web", "x", "social", "news", "images"];
/// Advertised Tavily search depths.
pub const VALID_SEARCH_DEPTHS: &[&str] = &["basic", "advanced", "fast", "ultra-fast"];
/// Deep modes are Exa server-side embeddings modes (B20/B29) — the deep
/// search leg triggers on `provider=exa` + one of these (or strategy=deep /
/// outputSchema). Non-exa providers never receive them (the product layer
/// maps them to `None` for web legs).
pub const VALID_DEEP_MODES: &[&str] = &["deep-lite", "deep", "deep-reasoning"];
/// Extract-only provider set: firecrawl/tavily/exa support extract (B10 adds
/// Exa `/contents`); `auto` lets the chain detect (firecrawl first).
pub const VALID_EXTRACT_PROVIDERS: &[&str] = &["auto", "tavily", "firecrawl", "exa"];

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

/// True when `value` is one of the Exa deep-search modes (B20/B29). The deep
/// modes are distinct from the Tavily depths in [`VALID_SEARCH_DEPTHS`]; they
/// select the Exa server-side embeddings leg.
pub fn is_deep_mode(value: Option<&str>) -> bool {
    matches!(
        value,
        Some("deep-lite") | Some("deep") | Some("deep-reasoning")
    )
}

/// Validate a `search_depth` knob: either a Tavily depth (`basic`/`advanced`/
/// `fast`/`ultra-fast`) or an Exa deep mode (`deep-lite`/`deep`/
/// `deep-reasoning`). Deep modes select the Exa server-side embeddings leg
/// (B20/B29); the product layer never forwards them to a web provider.
pub fn validate_search_depth(field: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        None | Some("") => Ok(()),
        Some(v) if VALID_SEARCH_DEPTHS.contains(&v) || is_deep_mode(Some(v)) => Ok(()),
        Some(v) => Err(format!(
            "{field}: {v:?} is not a supported value (valid: {})",
            VALID_SEARCH_DEPTHS
                .iter()
                .chain(VALID_DEEP_MODES.iter())
                .copied()
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Validate every value of a `sources` list against [`VALID_SOURCES`].
/// Empty entries are tolerated (routing treats them as absent).
pub fn validate_sources(field: &str, values: &[String]) -> Result<(), String> {
    for v in values {
        if v.is_empty() {
            continue;
        }
        if !VALID_SOURCES.contains(&v.as_str()) {
            return Err(format!(
                "{field}: {v:?} is not a supported source (valid: {})",
                VALID_SOURCES.join(", ")
            ));
        }
    }
    Ok(())
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

    // ---- B11: sources allowlist (news/images) ----

    #[test]
    fn valid_sources_accept_web_x_social_news_images() {
        for s in ["web", "x", "social", "news", "images"] {
            assert!(VALID_SOURCES.contains(&s), "VALID_SOURCES must include {s}");
            assert!(validate_sources("sources", &[s.to_string()]).is_ok());
        }
    }

    #[test]
    fn validate_sources_rejects_unknown_value() {
        let err = validate_sources("sources", &["banana".to_string()])
            .expect_err("banana is not a source");
        assert!(err.contains("sources"), "{err}");
        assert!(err.contains("banana"), "{err}");
        assert!(err.contains("news"), "valid set listed: {err}");
    }

    #[test]
    fn validate_sources_tolerates_empty_and_unset() {
        assert!(validate_sources("sources", &[]).is_ok());
        assert!(validate_sources("sources", &["".to_string()]).is_ok());
        assert!(validate_sources("sources", &["web".into(), "".into()]).is_ok());
    }

    // ---- B10: Exa joins the extract provider set ----

    #[test]
    fn extract_providers_include_exa() {
        assert!(
            VALID_EXTRACT_PROVIDERS.contains(&"exa"),
            "exa must be a valid extract provider (B10)"
        );
        assert!(validate_choice("provider", Some("exa"), VALID_EXTRACT_PROVIDERS).is_ok());
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
