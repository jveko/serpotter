//! 6-gate search routing (mysearch routing.ts lean port).

mod resolve;
mod rules;

pub use resolve::{fallback_chain, resolve_intent, resolve_strategy};

use crate::types::SearchQuery;
use resolve::{rule_matches, sources_list};
use rules::{Rule, RULES};

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
    fn handle_filter_routes_xai() {
        let q = SearchQuery {
            query: "ai".into(),
            allowed_x_handles: Some(crate::types::VecOrOne::Many(vec!["elonmusk".into()])),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "xai");
    }

    #[test]
    fn bare_web_query_not_xai() {
        // Research web leg strips handles — remaining query must not Gate-3 to xAI.
        let q = SearchQuery {
            query: "ai".into(),
            include_domains: Some(crate::types::VecOrOne::Many(vec!["example.com".into()])),
            from_date: Some("2026-01-01".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_ne!(d.provider, "xai", "web-only filters must not force social: {d:?}");
    }

    #[test]
    fn fallback_chain_tavily() {
        assert_eq!(fallback_chain("tavily"), vec!["tavily", "exa", "firecrawl"]);
    }
}
