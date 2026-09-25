//! 5-gate search routing (mysearch routing.ts lean port).

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
    let has_x = sources.iter().any(|s| s == "x" || s == "social") || mode == Some("social");
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
    // (news/social/docs/github/pdf keep their dedicated rules below), and
    // never hijack explicit news/images source requests (B11: an explicit
    // source list wins over the deep/content heuristic).
    if (strategy == Strategy::Deep || q.include_content == Some(true))
        && !matches!(mode, Some("news" | "social" | "docs" | "github" | "pdf"))
        && !sources.iter().any(|s| s == "news" || s == "images")
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
    unreachable!("boundary-legal intents and modes all match a routing rule")
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

    // ---- B11: explicit sources=["news"] / ["images"] routing ----

    #[test]
    fn news_source_routes_tavily_news_topic() {
        // Explicit source wins over auto-detected intent: the query has no
        // news phrasing, yet sources=["news"] must route to the news topic.
        let q = SearchQuery {
            query: "rust async".into(),
            sources: Some(crate::types::Sources::One("news".into())),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.tavily_topic.as_deref(), Some("news"), "{d:?}");
        assert_eq!(d.reason, "News source", "{d:?}");
        assert_ne!(d.provider, "xai", "news is not social: {d:?}");
    }

    #[test]
    fn news_source_not_hybrid_web_only() {
        let q = SearchQuery {
            query: "markets".into(),
            sources: Some(crate::types::Sources::One("news".into())),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert!(!d.hybrid, "news is not web+x hybrid: {d:?}");
        assert_eq!(d.sources.as_deref(), Some(&["news".to_string()][..]));
    }

    #[test]
    fn images_source_routes_firecrawl_images_category() {
        let q = SearchQuery {
            query: "coffee beans".into(),
            sources: Some(crate::types::Sources::One("images".into())),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "firecrawl", "{d:?}");
        assert_eq!(
            d.firecrawl_categories.as_deref(),
            Some(&["images".to_string()][..]),
            "{d:?}"
        );
        assert_eq!(d.reason, "Image search", "{d:?}");
        assert_ne!(d.provider, "tavily", "images must not route web: {d:?}");
    }

    #[test]
    fn news_source_beats_content_heuristic() {
        // include_content=true + sources=["news"] must keep the news topic —
        // Gate 4 (content/deep) must not hijack an explicit news source.
        let q = SearchQuery {
            query: "rust async".into(),
            sources: Some(crate::types::Sources::One("news".into())),
            include_content: Some(true),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(d.tavily_topic.as_deref(), Some("news"), "{d:?}");
        assert_ne!(d.provider, "firecrawl");
    }

    #[test]
    fn images_source_beats_deep_strategy() {
        let q = SearchQuery {
            query: "coffee beans".into(),
            sources: Some(crate::types::Sources::One("images".into())),
            strategy: Some("deep".into()),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "firecrawl", "{d:?}");
        assert_eq!(
            d.firecrawl_categories.as_deref(),
            Some(&["images".to_string()][..]),
            "{d:?}"
        );
    }

    #[test]
    fn social_source_aliases_to_x() {
        // "social" as a source keeps the xAI social semantic (alias of "x").
        let q = SearchQuery {
            query: "ai".into(),
            sources: Some(crate::types::Sources::One("social".into())),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "xai", "{d:?}");
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

    #[test]
    fn web_mode_is_web_only_and_beats_x_handle_filter() {
        // `mode=web` is the explicit web-only dial, equivalent to sources=[web].
        // It therefore cannot be Gate-3-hijacked by handle filters.
        let q = SearchQuery {
            query: "ai".into(),
            mode: Some("web".into()),
            allowed_x_handles: Some(crate::types::VecOrOne::Many(vec!["elonmusk".into()])),
            ..Default::default()
        };
        let d = route_search(RouteInput { query: &q });
        assert_eq!(d.provider, "tavily", "{d:?}");
        assert_eq!(
            d.sources.as_deref(),
            Some(&["web".to_string()][..]),
            "{d:?}"
        );
        assert!(!d.hybrid, "{d:?}");

        for source in ["x", "social"] {
            let mut q = SearchQuery {
                query: "ai".into(),
                mode: Some("web".into()),
                sources: Some(crate::types::Sources::One(source.into())),
                ..Default::default()
            };
            q.canonicalize();
            let d = route_search(RouteInput { query: &q });
            assert_eq!(d.provider, "tavily", "source={source:?}, decision={d:?}");
            assert_eq!(
                d.sources.as_deref(),
                Some(&["web".to_string()][..]),
                "source={source:?}, decision={d:?}"
            );
            assert!(!d.hybrid, "source={source:?}, decision={d:?}");
        }
    }

    #[test]
    fn every_surviving_rule_is_reachable_with_boundary_legal_input() {
        // One case per RULES row. This table is deliberately exhaustive: adding
        // or removing a rule without updating a legal route_search case fails
        // here, while the deleted research/default rows are intentionally absent
        // because no boundary-legal request can select them.
        let cases = [
            (
                SearchQuery {
                    sources: Some(crate::types::Sources::One("news".into())),
                    ..Default::default()
                },
                "News source",
            ),
            (
                SearchQuery {
                    sources: Some(crate::types::Sources::One("images".into())),
                    ..Default::default()
                },
                "Image search",
            ),
            (
                SearchQuery {
                    mode: Some("news".into()),
                    ..Default::default()
                },
                "News search",
            ),
            (
                SearchQuery {
                    mode: Some("docs".into()),
                    ..Default::default()
                },
                "Document discovery",
            ),
            (
                SearchQuery {
                    mode: Some("github".into()),
                    ..Default::default()
                },
                "GitHub document discovery",
            ),
            (
                SearchQuery {
                    mode: Some("pdf".into()),
                    ..Default::default()
                },
                "PDF document discovery",
            ),
            (
                SearchQuery {
                    intent: Some("news".into()),
                    ..Default::default()
                },
                "News search (auto-detected)",
            ),
            (
                SearchQuery {
                    intent: Some("status".into()),
                    ..Default::default()
                },
                "Status search",
            ),
            (
                SearchQuery {
                    intent: Some("comparison".into()),
                    ..Default::default()
                },
                "Comparison search",
            ),
            (
                SearchQuery {
                    intent: Some("tutorial".into()),
                    ..Default::default()
                },
                "Tutorial search",
            ),
            (
                SearchQuery {
                    intent: Some("exploratory".into()),
                    ..Default::default()
                },
                "Exploratory search",
            ),
            (
                SearchQuery {
                    intent: Some("resource".into()),
                    ..Default::default()
                },
                "Resource discovery",
            ),
            (
                SearchQuery {
                    intent: Some("factual".into()),
                    ..Default::default()
                },
                "AI answer",
            ),
        ];

        assert_eq!(
            cases.len(),
            rules::RULES.len(),
            "pin one legal case per surviving rule"
        );
        for (mut query, expected_reason) in cases {
            query.query = "boundary legal query".into();
            query.canonicalize();
            let decision = route_search(RouteInput { query: &query });
            assert_eq!(
                decision.reason, expected_reason,
                "query={query:?}, decision={decision:?}"
            );
        }
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
}
