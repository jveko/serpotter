//! 6-gate search routing (mysearch routing.ts lean port).

use crate::types::SearchQuery;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    Fast,
    Balanced,
    Verify,
    Deep,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Fast => "fast",
            Strategy::Balanced => "balanced",
            Strategy::Verify => "verify",
            Strategy::Deep => "deep",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub provider: String,
    pub reason: String,
    pub tavily_topic: Option<String>,
    pub firecrawl_categories: Option<Vec<String>>,
    pub sources: Option<Vec<String>>,
    pub strategy: Strategy,
    pub intent: String,
    pub blend: bool,
    pub hybrid: bool,
}

#[derive(Debug, Clone)]
pub struct RouteInput<'a> {
    pub query: &'a SearchQuery,
}

struct Rule {
    priority: i32,
    provider: &'static str,
    reason: &'static str,
    tavily_topic: Option<&'static str>,
    firecrawl_categories: Option<&'static [&'static str]>,
    match_mode: Option<&'static str>,
    match_intent: Option<&'static str>,
    match_sources: Option<&'static str>,
}

const RULES: &[Rule] = &[
    Rule {
        priority: 100,
        provider: "xai",
        reason: "Social search",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: Some("social"),
        match_intent: None,
        match_sources: Some("x"),
    },
    Rule {
        priority: 90,
        provider: "tavily",
        reason: "News search",
        tavily_topic: Some("news"),
        firecrawl_categories: None,
        match_mode: Some("news"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 85,
        provider: "tavily",
        reason: "Document discovery",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: Some("docs"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 85,
        provider: "tavily",
        reason: "GitHub document discovery",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: Some("github"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 85,
        provider: "tavily",
        reason: "PDF document discovery",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: Some("pdf"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 85,
        provider: "tavily",
        reason: "News search (auto-detected)",
        tavily_topic: Some("news"),
        firecrawl_categories: None,
        match_mode: None,
        match_intent: Some("news"),
        match_sources: None,
    },
    Rule {
        priority: 85,
        provider: "tavily",
        reason: "Status search",
        tavily_topic: Some("news"),
        firecrawl_categories: None,
        match_mode: None,
        match_intent: Some("status"),
        match_sources: None,
    },
    Rule {
        priority: 80,
        provider: "firecrawl",
        reason: "Document search",
        tavily_topic: None,
        firecrawl_categories: Some(&["research"]),
        match_mode: Some("docs"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 70,
        provider: "tavily",
        reason: "Resource discovery",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: None,
        match_intent: Some("resource"),
        match_sources: None,
    },
    Rule {
        priority: 60,
        provider: "tavily",
        reason: "AI answer",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: None,
        match_intent: Some("factual"),
        match_sources: None,
    },
    Rule {
        priority: 50,
        provider: "tavily",
        reason: "Research",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: Some("research"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 40,
        provider: "firecrawl",
        reason: "GitHub document fetch",
        tavily_topic: None,
        firecrawl_categories: Some(&["github"]),
        match_mode: Some("github"),
        match_intent: None,
        match_sources: None,
    },
    Rule {
        priority: 10,
        provider: "tavily",
        reason: "Default web search",
        tavily_topic: None,
        firecrawl_categories: None,
        match_mode: None,
        match_intent: None,
        match_sources: Some("web"),
    },
];

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

fn has_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| text.contains(n))
}

fn sources_list(q: &SearchQuery) -> Vec<String> {
    q.sources
        .as_ref()
        .map(|s| s.as_list())
        .unwrap_or_default()
}

fn rule_matches(rule: &Rule, mode: Option<&str>, intent: &str, sources: &[String]) -> bool {
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

pub fn route_search(input: RouteInput<'_>) -> RouteDecision {
    let q = input.query;
    let sources = sources_list(q);
    let mode = q.mode.as_deref();
    let intent = resolve_intent(q);

    let hybrid = sources.iter().any(|s| s == "web") && sources.iter().any(|s| s == "x");
    let strategy = resolve_strategy(q, &intent, hybrid);

    // Gate 1: explicit provider
    if let Some(p) = q.provider.as_deref() {
        if p != "auto" {
            let provider = if p == "social" { "xai" } else { p };
            return RouteDecision {
                provider: provider.into(),
                reason: "Explicit provider".into(),
                tavily_topic: None,
                firecrawl_categories: None,
                sources: if sources.is_empty() {
                    None
                } else {
                    Some(sources)
                },
                strategy,
                intent,
                blend: false,
                hybrid: provider == "hybrid",
            };
        }
    }

    // Gate 2: hybrid web+x
    if hybrid {
        return RouteDecision {
            provider: "hybrid".into(),
            reason: "Hybrid web+x".into(),
            tavily_topic: None,
            firecrawl_categories: None,
            sources: Some(vec!["web".into(), "x".into()]),
            strategy,
            intent,
            blend: false,
            hybrid: true,
        };
    }

    // Gate 3: social / x handles
    let has_x = sources.iter().any(|s| s == "x") || mode == Some("social");
    let handle_filter = q
        .allowed_x_handles
        .as_ref()
        .map(|v| v.is_nonempty())
        .unwrap_or(false)
        || q
            .excluded_x_handles
            .as_ref()
            .map(|v| v.is_nonempty())
            .unwrap_or(false);
    if has_x || handle_filter {
        return RouteDecision {
            provider: "xai".into(),
            reason: "Social / X search".into(),
            tavily_topic: None,
            firecrawl_categories: None,
            sources: Some(vec!["x".into()]),
            strategy,
            intent,
            blend: false,
            hybrid: false,
        };
    }

    // Gate 4: content / deep
    if strategy == Strategy::Deep || q.include_content == Some(true) {
        return RouteDecision {
            provider: "firecrawl".into(),
            reason: "Content / deep".into(),
            tavily_topic: None,
            firecrawl_categories: Some(vec!["research".into()]),
            sources: None,
            strategy,
            intent,
            blend: false,
            hybrid: false,
        };
    }

    // Gate 5: route table (priority desc)
    let mut rules: Vec<&Rule> = RULES.iter().collect();
    rules.sort_by_key(|b| std::cmp::Reverse(b.priority));
    for rule in rules {
        if rule_matches(rule, mode, &intent, &sources) {
            let blend = matches!(strategy, Strategy::Balanced | Strategy::Verify)
                && (rule.provider == "tavily" || rule.provider == "firecrawl");
            return RouteDecision {
                provider: rule.provider.into(),
                reason: rule.reason.into(),
                tavily_topic: rule.tavily_topic.map(str::to_string),
                firecrawl_categories: rule
                    .firecrawl_categories
                    .map(|c| c.iter().map(|s| (*s).to_string()).collect()),
                sources: if sources.is_empty() {
                    None
                } else {
                    Some(sources.clone())
                },
                strategy,
                intent,
                blend,
                hybrid: false,
            };
        }
    }

    // Gate 6: fallback tavily
    let blend = matches!(strategy, Strategy::Balanced | Strategy::Verify);
    RouteDecision {
        provider: "tavily".into(),
        reason: "Fallback tavily".into(),
        tavily_topic: None,
        firecrawl_categories: None,
        sources: None,
        strategy,
        intent,
        blend,
        hybrid: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SearchQuery;

    #[test]
    fn explicit_provider() {
        let q = SearchQuery {
            query: "hi".into(),
            provider: Some("exa".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "exa");
    }

    #[test]
    fn news_mode_tavily_topic() {
        let q = SearchQuery {
            query: "markets".into(),
            mode: Some("news".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily");
        assert_eq!(d.tavily_topic.as_deref(), Some("news"));
    }

    #[test]
    fn hybrid_web_x() {
        let q = SearchQuery {
            query: "hi".into(),
            sources: Some(crate::types::Sources::Many(vec!["web".into(), "x".into()])),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert!(d.hybrid);
        assert_eq!(d.provider, "hybrid");
    }

    #[test]
    fn fallback_chain_tavily() {
        assert_eq!(fallback_chain("tavily"), vec!["tavily", "exa", "firecrawl"]);
    }
}
