//! Search orchestration (multi-provider routing + RRF). No HTTP / auth.

mod banned;
mod exhausted;
mod execute;
mod leg_errors;
mod run_provider;

pub use banned::is_firecrawl_banned;
pub use exhausted::is_exhausted_status;
pub use leg_errors::{first_blend_err, hybrid_leg_errors, multi_leg_errors};
pub use run_provider::run_provider;

use serpotter_core::{route_search, RouteDebug, RouteInput, SearchQuery, SearchResponse};

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

    let strategy_label = if decision.hybrid {
        "hybrid"
    } else if decision.blend {
        if decision.strategy.as_str() == "verify" || decision.strategy.as_str().contains("verify") {
            "verify"
        } else {
            "blend"
        }
    } else {
        "single"
    };

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

    match &mut outcome {
        Ok(o) => {
            o.meta.strategy = Some(strategy_label.into());
            o.result.route_debug = Some(RouteDebug {
                intent: Some(decision.intent.clone()),
                strategy: Some(decision.strategy.as_str().into()),
                reason: Some(decision.reason.clone()),
            });
        }
        Err(o) => {
            o.meta.strategy = Some(strategy_label.into());
        }
    }
    outcome
}
