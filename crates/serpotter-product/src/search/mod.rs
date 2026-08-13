//! Search orchestration (multi-provider routing + RRF). No HTTP / auth.

mod banned;
mod chain;
mod execute;
mod exhausted;
mod leg_errors;
mod run_provider;

pub use banned::is_firecrawl_banned;
pub use exhausted::is_exhausted_status;
pub use leg_errors::{first_blend_err, multi_leg_errors};
pub use run_provider::{map_lease_err, run_provider};

use serpotter_core::{
    is_deep_mode, route_search, RouteDebug, RouteInput, SearchQuery, SearchResponse,
};
use serpotter_providers::{SVC_EXA, SVC_TAVILY};

use crate::cache::{self, SERVICE_SEARCH};
use crate::error::SearchExecError;
use crate::meta::ProductOutcome;
use crate::ProductCtx;

use execute::{execute_blend, execute_deep_search, execute_hybrid, execute_single_chain};

/// Which execution path `search_inner` runs for a routed request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanKind {
    /// Exa server-side embeddings leg (deep search + optional structured
    /// synthesis): `provider=exa` with `outputSchema`, a deep `search_depth`
    /// (deep-lite|deep|deep-reasoning) or `strategy=deep`.
    Deep,
    /// Web chain + x social leg merge.
    Hybrid,
    /// Multiple provider legs merged (Balanced/Verify blend).
    Blend,
    /// One fallback chain.
    Single,
}

impl PlanKind {
    /// Lowercase dial label, used in `route_debug.reason`.
    fn label(self) -> &'static str {
        match self {
            PlanKind::Deep => "deep",
            PlanKind::Hybrid => "hybrid",
            PlanKind::Blend => "blend",
            PlanKind::Single => "single",
        }
    }
}

/// Decide the execution plan for a routed request. Pure: same inputs always
/// yield the same plan, so the dispatch in `search_inner` (and the tests) can
/// reason about it without re-deriving conditions.
///
/// Returns the plan kind plus the primary provider label for that plan:
/// - Deep: the Exa embeddings leg — provider is always `exa` (the trigger
///   requires it and Gate 1 preserves explicit providers).
/// - Hybrid: the web leg always runs the tavily-headed chain (execute_hybrid),
///   so "tavily" is the primary label even though the routed provider string
///   is "hybrid".
/// - Blend / Single: the routed primary provider (`decision.provider`).
pub(crate) fn execution_plan(
    decision: &serpotter_core::RouteDecision,
    body: &SearchQuery,
) -> (PlanKind, String) {
    // B20/B29: explicit provider=exa with `outputSchema`, a deep
    // `search_depth` (deep-lite|deep|deep-reasoning) or `strategy=deep`
    // replaces the normal execute paths with the Exa server-side embeddings
    // leg (deep search + optional structured synthesis).
    let deep = body.provider.as_deref() == Some(SVC_EXA)
        && (body.output_schema.is_some()
            || is_deep_mode(body.search_depth.as_deref())
            || body.strategy.as_deref() == Some("deep"));
    if deep {
        return (PlanKind::Deep, SVC_EXA.to_string());
    }
    if decision.hybrid {
        return (PlanKind::Hybrid, SVC_TAVILY.to_string());
    }
    if decision.blend {
        return (PlanKind::Blend, decision.provider.clone());
    }
    (PlanKind::Single, decision.provider.clone())
}

/// Public search used by HTTP handlers / MCP / research (auth already checked).
pub async fn search_inner(
    ctx: &ProductCtx,
    body: SearchQuery,
) -> Result<ProductOutcome<SearchResponse>, ProductOutcome<SearchExecError>> {
    if body.query.trim().is_empty() {
        return Err(ProductOutcome {
            result: SearchExecError::Search("missing_query".into()),
            meta: Default::default(),
        });
    }

    // B1: exact-query TTL cache. The key covers the FULL request shape, so the
    // cache is exact (never a fuzzy match). A hit serves the stored response
    // with zero provider calls; meta is a synthetic cache marker (strategy
    // "cache", no attempts, cache_hit=true) so request_log/metrics can tell
    // cache serves apart. Fail-open: any cache fault is a miss.
    let canonical = cache::canonical_query(&body);
    if let Some(json) = cache::cache_get(ctx, SERVICE_SEARCH, &canonical).await {
        if let Ok(mut resp) = serde_json::from_str::<SearchResponse>(&json) {
            resp.cache_hit = Some(true);
            let mut meta = crate::meta::ExecMeta::default();
            // "cache" is a serving marker, not a routed strategy or an
            // execute-path dial label — it exists so request_log rows for
            // cache-served requests are self-describing (F16 compares raw vs
            // dial; this is neither).
            meta.strategy = Some("cache".into());
            meta.mark_cache_hit();
            return Ok(ProductOutcome { result: resp, meta });
        }
    }

    let decision = route_search(RouteInput { query: &body });
    let max_results = body.clamped_max_results();
    let include_content = body.include_content.unwrap_or(false);
    let include_domains = body
        .include_domains
        .as_ref()
        .map(|v| v.as_list())
        .unwrap_or_default();
    let exclude_domains = body
        .exclude_domains
        .as_ref()
        .map(|v| v.as_list())
        .unwrap_or_default();

    // C2c: explicit execution plan. Deep when provider=exa with an output
    // schema, a deep `search_depth` or `strategy=deep` (B20/B29, trigger rules
    // unchanged); otherwise Hybrid/Blend/Single straight from the decision.
    let (plan_kind, primary_label) = execution_plan(&decision, &body);

    let mut outcome = match plan_kind {
        PlanKind::Deep => execute_deep_search(ctx, &body, max_results).await,
        PlanKind::Hybrid => {
            execute_hybrid(
                ctx,
                &body,
                &decision,
                max_results,
                include_content,
                &include_domains,
                &exclude_domains,
            )
            .await
        }
        PlanKind::Blend => {
            execute_blend(
                ctx,
                &body,
                &decision,
                max_results,
                include_content,
                &include_domains,
                &exclude_domains,
            )
            .await
        }
        PlanKind::Single => {
            execute_single_chain(
                ctx,
                &body,
                &decision,
                max_results,
                include_content,
                &include_domains,
                &exclude_domains,
            )
            .await
        }
    };

    // F16: request_log `strategy` stores the RAW routed strategy
    // (auto/fast/balanced/verify/deep as routed), never the execute-path dial
    // label ("hybrid"/"blend"/"single") — docs/ops/api.md documents the column
    // as the raw routing strategy, so the persisted value must make that true.
    let raw_strategy = decision.strategy.as_str();
    match &mut outcome {
        Ok(o) => {
            o.meta.strategy = Some(raw_strategy.into());
            // C2c: human-readable reason naming the executed plan kind and its
            // primary provider ("single tavily", "blend firecrawl",
            // "hybrid tavily", "deep exa") so agents see the plan at a glance.
            o.result.route_debug = Some(RouteDebug {
                intent: Some(decision.intent.clone()),
                strategy: Some(raw_strategy.into()),
                reason: Some(format!("{} {}", plan_kind.label(), primary_label)),
            });
        }
        Err(o) => {
            o.meta.strategy = Some(raw_strategy.into());
        }
    }

    // B1: store only successful responses (never error shapes). Stored JSON
    // omits cache_hit (None at this point), so the round-trip is clean. Runs
    // AFTER the F16 block so route_debug + raw strategy are part of the cached
    // payload (cache serves stay as informative as live ones).
    if let Ok(o) = &outcome {
        if let Ok(json) = serde_json::to_string(&o.result) {
            cache::cache_put(ctx, SERVICE_SEARCH, &canonical, &json).await;
        }
    }

    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_core::{route_search, RouteInput, Sources};

    fn routed(body: SearchQuery) -> serpotter_core::RouteDecision {
        route_search(RouteInput { query: &body })
    }

    // --- Deep trigger matrix (B20/B29 rules, unchanged from the old if/else) --

    #[test]
    fn plan_is_deep_for_output_schema_only() {
        let body = SearchQuery {
            query: "structured".into(),
            provider: Some("exa".into()),
            output_schema: Some(serde_json::json!({"type": "object"})),
            ..Default::default()
        };
        let (kind, primary) = execution_plan(&routed(body.clone()), &body);
        assert_eq!(kind, PlanKind::Deep);
        assert_eq!(primary, "exa");
    }

    #[test]
    fn plan_is_deep_for_search_depth() {
        for depth in ["deep-lite", "deep", "deep-reasoning"] {
            let body = SearchQuery {
                query: "deep".into(),
                provider: Some("exa".into()),
                search_depth: Some(depth.into()),
                ..Default::default()
            };
            let (kind, primary) = execution_plan(&routed(body.clone()), &body);
            assert_eq!(kind, PlanKind::Deep, "search_depth={depth}");
            assert_eq!(primary, "exa");
        }
    }

    #[test]
    fn plan_is_deep_for_strategy_deep() {
        let body = SearchQuery {
            query: "strategy deep".into(),
            provider: Some("exa".into()),
            strategy: Some("deep".into()),
            ..Default::default()
        };
        let (kind, primary) = execution_plan(&routed(body.clone()), &body);
        assert_eq!(kind, PlanKind::Deep);
        assert_eq!(primary, "exa");
    }

    #[test]
    fn plan_is_single_for_exa_without_deep_trigger() {
        let body = SearchQuery {
            query: "plain exa".into(),
            provider: Some("exa".into()),
            ..Default::default()
        };
        let (kind, primary) = execution_plan(&routed(body.clone()), &body);
        assert_eq!(
            kind,
            PlanKind::Single,
            "no outputSchema/depth/strategy=deep"
        );
        assert_eq!(primary, "exa");
    }

    // --- Hybrid / Blend / Single come straight from the decision --------------

    #[test]
    fn plan_is_hybrid_for_web_x_sources() {
        let body = SearchQuery {
            query: "hybrid".into(),
            sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
            ..Default::default()
        };
        let decision = routed(body.clone());
        assert!(decision.hybrid, "gate 2 must set hybrid: {decision:?}");
        let (kind, primary) = execution_plan(&decision, &body);
        assert_eq!(kind, PlanKind::Hybrid);
        assert_eq!(
            primary, "tavily",
            "hybrid web leg runs the tavily-headed chain"
        );
    }

    #[test]
    fn plan_is_blend_for_balanced_strategy() {
        let body = SearchQuery {
            query: "blend".into(),
            strategy: Some("balanced".into()),
            ..Default::default()
        };
        let decision = routed(body.clone());
        assert!(decision.blend, "balanced fallback must blend: {decision:?}");
        let (kind, primary) = execution_plan(&decision, &body);
        assert_eq!(kind, PlanKind::Blend);
        assert_eq!(primary, "tavily");
    }

    #[test]
    fn plan_is_single_for_default_query() {
        let body = SearchQuery {
            query: "hello".into(),
            ..Default::default()
        };
        let decision = routed(body.clone());
        assert!(!decision.hybrid && !decision.blend);
        let (kind, primary) = execution_plan(&decision, &body);
        assert_eq!(kind, PlanKind::Single);
        assert_eq!(primary, "tavily");
    }

    #[test]
    fn plan_kind_labels_are_lowercase_dial_labels() {
        assert_eq!(PlanKind::Deep.label(), "deep");
        assert_eq!(PlanKind::Hybrid.label(), "hybrid");
        assert_eq!(PlanKind::Blend.label(), "blend");
        assert_eq!(PlanKind::Single.label(), "single");
    }
}

#[cfg(test)]
mod happy_path_tests;
#[cfg(test)]
mod progress_tests;
