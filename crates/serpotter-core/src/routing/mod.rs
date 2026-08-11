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
        || q.excluded_x_handles
            .as_ref()
            .map(|v| v.is_nonempty())
            .unwrap_or(false);
    if has_x || (handle_filter && sources.is_empty()) {
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

    // Gate 4: content / deep — but never hijack modes the route table serves
    // (news/social/docs/github/pdf keep their dedicated rules below).
    if (strategy == Strategy::Deep || q.include_content == Some(true))
        && !matches!(mode, Some("news" | "social" | "docs" | "github" | "pdf"))
    {
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

    // Gate 6: fallback tavily — reason must be truthful about the strategy:
    // a Balanced/Verify strategy here is a blend, not a plain fallback.
    let blend = matches!(strategy, Strategy::Balanced | Strategy::Verify);
    let reason = match strategy {
        Strategy::Balanced => "Balanced blend",
        Strategy::Verify => "Verify blend",
        _ => "Fallback tavily",
    };
    RouteDecision {
        provider: "tavily".into(),
        reason: reason.into(),
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
        assert_ne!(
            d.provider, "xai",
            "web-only filters must not force social: {d:?}"
        );
    }

    #[test]
    fn fallback_chain_tavily() {
        assert_eq!(fallback_chain("tavily"), vec!["tavily", "exa", "firecrawl"]);
    }

    // ---- B1: strategy="auto" must derive, not silently pin Fast ----

    #[test]
    fn strategy_auto_derives_from_intent() {
        let q = SearchQuery {
            strategy: Some("auto".into()),
            intent: Some("comparison".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_strategy(&q, "comparison", false),
            Strategy::Verify,
            "auto + comparison must derive Verify"
        );
        let q = SearchQuery {
            strategy: Some("auto".into()),
            intent: Some("factual".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_strategy(&q, "factual", false),
            Strategy::Fast,
            "auto + factual must derive Fast"
        );
    }

    #[test]
    fn strategy_auto_derives_from_hybrid_and_mode() {
        let q = SearchQuery {
            strategy: Some("auto".into()),
            sources: Some(crate::types::Sources::Many(vec!["web".into(), "x".into()])),
            ..Default::default()
        };
        assert_eq!(
            resolve_strategy(&q, "factual", true),
            Strategy::Balanced,
            "auto + hybrid web+x must derive Balanced"
        );
        let q = SearchQuery {
            strategy: Some("auto".into()),
            mode: Some("research".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_strategy(&q, "exploratory", false),
            Strategy::Deep,
            "auto + mode=research must derive Deep"
        );
    }

    #[test]
    fn unknown_explicit_strategy_stays_fast() {
        let q = SearchQuery {
            strategy: Some("banana".into()),
            ..Default::default()
        };
        assert_eq!(
            resolve_strategy(&q, "factual", false),
            Strategy::Fast,
            "unknown explicit strategy must keep the old Fast behavior"
        );
    }

    // ---- B2: comparison/tutorial intent precedence over news ----

    #[test]
    fn intent_how_to_update_is_tutorial_not_news() {
        let q = SearchQuery {
            query: "how to update to react 19".into(),
            ..Default::default()
        };
        assert_eq!(
            resolve_intent(&q),
            "tutorial",
            "weak news signal 'update' must not win over 'how to'"
        );
    }

    #[test]
    fn intent_latest_release_is_news() {
        let q = SearchQuery {
            query: "latest react 19 release".into(),
            ..Default::default()
        };
        assert_eq!(resolve_intent(&q), "news");
    }

    #[test]
    fn intent_which_is_better_is_comparison() {
        let q = SearchQuery {
            query: "which is better rust or go".into(),
            ..Default::default()
        };
        assert_eq!(resolve_intent(&q), "comparison");
    }

    // ---- B3: Gate 3 must not hijack explicit web sources ----

    #[test]
    fn explicit_web_sources_beat_handle_filter() {
        let q = SearchQuery {
            query: "ai".into(),
            sources: Some(crate::types::Sources::One("web".into())),
            allowed_x_handles: Some(crate::types::VecOrOne::Many(vec!["elonmusk".into()])),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(
            d.provider, "tavily",
            "explicit sources=[web] + handle filter must stay on the web provider: {d:?}"
        );
    }

    #[test]
    fn handle_filter_without_sources_still_routes_xai() {
        let q = SearchQuery {
            query: "ai".into(),
            excluded_x_handles: Some(crate::types::VecOrOne::Many(vec!["spam".into()])),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "xai");
        assert_eq!(d.sources.as_deref(), Some(&["x".to_string()][..]));
    }

    // ---- B4: Gate 4 must not hijack modes Gate 5 serves ----

    #[test]
    fn news_mode_with_include_content_keeps_news_topic() {
        let q = SearchQuery {
            query: "markets".into(),
            mode: Some("news".into()),
            include_content: Some(true),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.tavily_topic.as_deref(), Some("news"));
        assert_ne!(d.provider, "firecrawl");
    }

    #[test]
    fn docs_mode_with_deep_strategy_keeps_document_discovery() {
        let q = SearchQuery {
            query: "axum docs".into(),
            mode: Some("docs".into()),
            strategy: Some("deep".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.reason, "Document discovery");
    }

    #[test]
    fn no_mode_with_deep_still_routes_firecrawl_research() {
        let q = SearchQuery {
            query: "quantum computing".into(),
            strategy: Some("deep".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "firecrawl", "{d:?}");
        assert_eq!(
            d.firecrawl_categories.as_deref(),
            Some(&["research".to_string()][..])
        );
    }

    // ---- B5: intent rules + truthful Gate 6 reasons ----

    #[test]
    fn comparison_query_routes_tavily_comparison_reason_verify_blend() {
        let q = SearchQuery {
            query: "which is better rust or go".into(),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.reason, "Comparison search");
        assert_eq!(d.strategy, Strategy::Verify);
        assert!(d.blend);
    }

    #[test]
    fn tutorial_query_routes_tavily_tutorial_reason_balanced_blend() {
        let q = SearchQuery {
            query: "how to deploy a rust service".into(),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.reason, "Tutorial search");
        assert_eq!(d.strategy, Strategy::Balanced);
        assert!(d.blend);
    }

    #[test]
    fn exploratory_query_routes_tavily_exploratory_reason() {
        let q = SearchQuery {
            query: "why are black holes stable".into(),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.reason, "Exploratory search");
    }

    #[test]
    fn gate6_reason_is_truthful_per_strategy() {
        // An explicit intent outside the rule set falls through to Gate 6; the
        // reason must reflect the strategy, not always claim a plain fallback.
        let q = SearchQuery {
            query: "hello".into(),
            intent: Some("banana".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.reason, "Fallback tavily");
        assert_eq!(d.strategy, Strategy::Fast);
        assert!(!d.blend);

        let q = SearchQuery {
            query: "hello".into(),
            intent: Some("banana".into()),
            strategy: Some("balanced".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.reason, "Balanced blend");
        assert!(d.blend);

        let q = SearchQuery {
            query: "hello".into(),
            intent: Some("banana".into()),
            strategy: Some("verify".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.reason, "Verify blend");
        assert!(d.blend);
    }
}
