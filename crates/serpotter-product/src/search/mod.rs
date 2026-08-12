//! Search orchestration (multi-provider routing + RRF). No HTTP / auth.

mod banned;
mod execute;
mod exhausted;
mod leg_errors;
mod run_provider;

pub use banned::is_firecrawl_banned;
pub use exhausted::is_exhausted_status;
pub use leg_errors::{first_blend_err, multi_leg_errors};
pub use run_provider::run_provider;

use serpotter_core::{route_search, RouteDebug, RouteInput, SearchQuery, SearchResponse};

use crate::cache::{self, SERVICE_SEARCH};
use crate::error::SearchExecError;
use crate::meta::ProductOutcome;
use crate::ProductCtx;

use execute::{execute_blend, execute_hybrid, execute_single_chain};

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

    let mut outcome = if decision.hybrid {
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
    } else if decision.blend {
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
    } else {
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
    };

    // F16: request_log `strategy` stores the RAW routed strategy
    // (auto/fast/balanced/verify/deep as routed), never the execute-path dial
    // label ("hybrid"/"blend"/"single") — docs/ops/api.md documents the column
    // as the raw routing strategy, so the persisted value must make that true.
    let raw_strategy = decision.strategy.as_str();
    match &mut outcome {
        Ok(o) => {
            o.meta.strategy = Some(raw_strategy.into());
            o.result.route_debug = Some(RouteDebug {
                intent: Some(decision.intent.clone()),
                strategy: Some(raw_strategy.into()),
                reason: Some(decision.reason.clone()),
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
mod happy_path_tests;
#[cfg(test)]
mod progress_tests;
