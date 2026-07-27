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
    if has_any(
        &text,
        &[
            "just now", "latest", "news", "update", "release", "announc", "breaking",
        ],
    ) && !has_any(
        &text,
        &["breaking change", "latest version"],
    ) {
        return "news".into();
    }
    if has_any(
        &text,
        &["vs.", "versus", "compare", "difference", "which is better", "pros and cons"],
    ) {
        return "comparison".into();
    }
    if has_any(
        &text,
        &["how to", "guide", "tutorial", "getting started", "step by step", "walkthrough"],
    ) {
        return "tutorial".into();
    }
    if has_any(
        &text,
        &["docs", "documentation", "api", "pricing", "readme", "reference", "spec"],
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
        return match s {
            "balanced" => Strategy::Balanced,
            "verify" => Strategy::Verify,
            "deep" => Strategy::Deep,
            "fast" => Strategy::Fast,
            _ => Strategy::Fast,
        };
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
    if matches!(
        q.mode.as_deref(),
        Some("docs" | "github" | "pdf")
    ) || intent == "resource"
        || intent == "tutorial"
    {
        return Strategy::Balanced;
    }
    Strategy::Fast
}

pub(crate) fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

pub(crate) fn sources_list(q: &SearchQuery) -> Vec<String> {
    q.sources
        .as_ref()
        .map(|s| s.as_list())
        .unwrap_or_default()
}

pub(crate) fn rule_matches(rule: &Rule, mode: Option<&str>, intent: &str, sources: &[String]) -> bool {
    if let Some(m) = rule.match_mode {
        if mode != Some(m) {
            // allow match_sources alone for social/web rules without mode
            if rule.match_sources.is_none() {
                return false;
            }
            if mode.is_some() {
                return false;
            }
        }
    }
    if let Some(i) = rule.match_intent {
        if intent != i {
            return false;
        }
    }
    if let Some(src) = rule.match_sources {
        if !sources.iter().any(|s| s == src) && mode != Some(if src == "x" { "social" } else { "" })
        {
            // mode social matches sources x rule
            if !(src == "x" && mode == Some("social")) {
                if sources.is_empty() && src == "web" && mode.is_none() {
                    // default web rule only if explicitly web or empty default later
                    return false;
                }
                if !sources.iter().any(|s| s == src) {
                    return false;
                }
            }
        }
    }
    // pure mode rules: match_mode set and mode equals
    if let Some(m) = rule.match_mode {
        if mode == Some(m) {
            return true;
        }
        if rule.match_sources.is_some() && sources.iter().any(|s| Some(s.as_str()) == rule.match_sources)
        {
            return true;
        }
        return mode == Some(m);
    }
    if rule.match_intent.is_some() {
        return true;
    }
    if let Some(src) = rule.match_sources {
        return sources.iter().any(|s| s == src) || (src == "x" && mode == Some("social"));
    }
    false
}

/// Fallback provider chain for execute-single.
pub fn fallback_chain(provider: &str) -> Vec<&'static str> {
    match provider {
        "tavily" => vec!["tavily", "exa", "firecrawl"],
        "firecrawl" => vec!["firecrawl", "exa", "tavily"],
        "exa" => vec!["exa", "firecrawl", "tavily"],
        "xai" => vec!["xai"],
        "hybrid" => vec!["tavily", "xai"],
        other => {
            // unknown: just itself if known-ish
            let _ = other;
            vec!["tavily", "exa", "firecrawl"]
        }
    }
}
