use super::rules::Rule;
use super::Strategy;
use crate::types::SearchQuery;

pub fn resolve_intent(q: &SearchQuery) -> String {
    if let Some(i) = q.intent.as_deref() {
        if i != "auto" {
            return i.to_string();
        }
    }
    if let Some(mode) = q.mode.as_deref() {
        match mode {
            "news" => return "news".into(),
            "docs" | "github" | "pdf" => return "resource".into(),
            "research" => return "exploratory".into(),
            _ => {}
        }
    }
    let text = q.query.to_lowercase();
    // Comparison and tutorial checks run BEFORE the news keyword check: weak
    // news signals ("update", "release") must not override explicit how-to /
    // comparison phrasing ("how to update to react 19" is a tutorial, not news).
    if has_any(
        &text,
        &[
            "vs.",
            "versus",
            "compare",
            "difference",
            "which is better",
            "pros and cons",
        ],
    ) {
        return "comparison".into();
    }
    if has_any(
        &text,
        &[
            "how to",
            "guide",
            "tutorial",
            "getting started",
            "step by step",
            "walkthrough",
        ],
    ) {
        return "tutorial".into();
    }
    if has_any(
        &text,
        &[
            "just now", "latest", "news", "update", "release", "announc", "breaking",
        ],
    ) && !has_any(&text, &["breaking change", "latest version"])
    {
        return "news".into();
    }
    if has_any(
        &text,
        &[
            "docs",
            "documentation",
            "api",
            "pricing",
            "readme",
            "reference",
            "spec",
        ],
    ) {
        return "resource".into();
    }
    if has_any(
        &text,
        &["status", "incident", "outage", "roadmap", "changelog"],
    ) {
        return "status".into();
    }
    if has_any(&text, &["why ", "explain", "overview"]) {
        return "exploratory".into();
    }
    "factual".into()
}

pub fn resolve_strategy(q: &SearchQuery, intent: &str, hybrid: bool) -> Strategy {
    if let Some(s) = q.strategy.as_deref() {
        match s {
            "balanced" => return Strategy::Balanced,
            "verify" => return Strategy::Verify,
            "deep" => return Strategy::Deep,
            "fast" => return Strategy::Fast,
            // "auto" (and None) mean auto-detect: fall through to the
            // intent/hybrid/mode heuristics instead of silently pinning Fast.
            "auto" => {}
            // Unknown explicit strings stay Fast (both surfaces' normalize_choice
            // already restrict the closed sets; this arm is the last-resort
            // coercion for a value that reached routing some other way).
            _ => return Strategy::Fast,
        }
    }
    if hybrid {
        return Strategy::Balanced;
    }
    if q.mode.as_deref() == Some("research") {
        return Strategy::Deep;
    }
    if intent == "comparison" || intent == "exploratory" {
        return Strategy::Verify;
    }
    if matches!(q.mode.as_deref(), Some("docs" | "github" | "pdf"))
        || intent == "resource"
        || intent == "tutorial"
    {
        return Strategy::Balanced;
    }
    Strategy::Fast
}

pub(crate) fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

/// `mode=web` overrides every explicit source list with the single web leg.
pub(crate) fn sources_list(q: &SearchQuery) -> Vec<String> {
    if q.mode.as_deref() == Some("web") {
        return vec!["web".into()];
    }
    q.sources.as_ref().map(|s| s.as_list()).unwrap_or_default()
}

pub(crate) fn rule_matches(
    rule: &Rule,
    mode: Option<&str>,
    intent: &str,
    sources: &[String],
) -> bool {
    rule.match_mode
        .is_none_or(|expected| mode == Some(expected))
        && rule.match_intent.is_none_or(|expected| intent == expected)
        && rule
            .match_sources
            .is_none_or(|expected| sources.iter().any(|source| source == expected))
}

/// Fallback provider chain for execute-single.
pub fn fallback_chain(provider: &str) -> Vec<&'static str> {
    match provider {
        "tavily" => vec!["tavily", "exa", "firecrawl"],
        "firecrawl" => vec!["firecrawl", "exa", "tavily"],
        "exa" => vec!["exa", "firecrawl", "tavily"],
        "xai" => vec!["xai"],
        _ => {
            // Fallback callers pass an already-selected single-provider chain;
            // hybrid is dispatched separately before this function is used.
            vec!["tavily", "exa", "firecrawl"]
        }
    }
}
