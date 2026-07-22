//! Search orchestration (multi-provider routing + RRF). No HTTP / auth.

use serpotter_core::{
    fallback_chain, reciprocal_rank_fusion, route_search, RouteDebug, RouteInput, RrfList,
    SearchQuery, SearchResponse, Strategy,
};
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{
    ProviderError, ProviderResult, ProviderSearchParams, SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI,
};

use crate::error::SearchExecError;
use crate::ProductCtx;

async fn execute_single_chain(
    ctx: &ProductCtx,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<SearchResponse, SearchExecError> {
    let chain = fallback_chain(&decision.provider);
    let mut last_err = SearchExecError::NoHealthyKey("No healthy provider key".into());

    for provider in chain {
        match run_provider(
            ctx,
            provider,
            body,
            decision,
            max_results,
            include_content,
            include_domains,
            exclude_domains,
            decision.sources.as_deref(),
        )
        .await
        {
            Ok(r) => {
                return Ok(r.into_search_response());
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn execute_hybrid(
    ctx: &ProductCtx,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<SearchResponse, SearchExecError> {
    let web_src = ["web".to_string()];
    let x_src = ["x".to_string()];
    let web = run_provider(
        ctx,
        SVC_TAVILY,
        body,
        decision,
        max_results,
        include_content,
        include_domains,
        exclude_domains,
        Some(web_src.as_slice()),
    )
    .await;
    let x_max = max_results.min(5);
    let x = run_provider(
        ctx,
        SVC_XAI,
        body,
        decision,
        x_max,
        false,
        include_domains,
        exclude_domains,
        Some(x_src.as_slice()),
    )
    .await;

    let web_items = web.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let x_items = x.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    if web_items.is_empty() && x_items.is_empty() {
        return Err(web.err().or(x.err()).unwrap_or(SearchExecError::Search(
            "hybrid both legs empty".into(),
        )));
    }
    let merged = reciprocal_rank_fusion(&[
        RrfList {
            items: web_items,
            weight: 1.0,
        },
        RrfList {
            items: x_items,
            weight: 0.7,
        },
    ]);
    let items: Vec<_> = merged.into_iter().take(max_results as usize).collect();
    Ok(SearchResponse {
        query: body.query.clone(),
        provider_used: "hybrid".into(),
        items,
        answer: web.ok().and_then(|r| r.answer),
        route_debug: None,
    })
}

async fn execute_blend(
    ctx: &ProductCtx,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<SearchResponse, SearchExecError> {
    let primary = decision.provider.as_str();
    let secondary = if primary == SVC_FIRECRAWL {
        SVC_TAVILY
    } else {
        SVC_FIRECRAWL
    };

    let a = run_provider(
        ctx,
        primary,
        body,
        decision,
        max_results,
        include_content,
        include_domains,
        exclude_domains,
        None,
    )
    .await;
    let b = run_provider(
        ctx,
        secondary,
        body,
        decision,
        max_results,
        include_content,
        include_domains,
        exclude_domains,
        None,
    )
    .await;

    let c = if decision.strategy == Strategy::Verify {
        Some(
            run_provider(
                ctx,
                "exa",
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                None,
            )
            .await,
        )
    } else {
        None
    };

    let a_items = a.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let b_items = b.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let c_items = c
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|r| r.items.as_slice())
        .unwrap_or(&[]);

    if a_items.is_empty() && b_items.is_empty() && c_items.is_empty() {
        return Err(a
            .err()
            .or(b.err())
            .unwrap_or(SearchExecError::Search("blend empty".into())));
    }

    let mut lists = vec![
        RrfList {
            items: a_items,
            weight: 1.0,
        },
        RrfList {
            items: b_items,
            weight: 0.7,
        },
    ];
    if !c_items.is_empty() {
        lists.push(RrfList {
            items: c_items,
            weight: 0.7,
        });
    }
    let merged = reciprocal_rank_fusion(&lists);
    let items: Vec<_> = merged.into_iter().take(max_results as usize).collect();
    let answer = a.ok().and_then(|r| r.answer);
    Ok(SearchResponse {
        query: body.query.clone(),
        provider_used: if decision.strategy == Strategy::Verify {
            "blend-verify".into()
        } else {
            "blend".into()
        },
        items,
        answer,
        route_debug: None,
    })
}

/// Run one provider: acquire a small key batch and try keys sequentially.
#[allow(clippy::too_many_arguments)]
pub async fn run_provider(
    ctx: &ProductCtx,
    provider: &str,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
    sources_override: Option<&[String]>,
) -> Result<ProviderResult, SearchExecError> {
    let batch = match ctx.keys.acquire_batch(provider, 3).await {
        Ok(b) => b,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(SearchExecError::NoHealthyKey(format!(
                "No healthy {s} key"
            )));
        }
        Err(KeyPoolError::Db(e)) => {
            return Err(SearchExecError::Db(e));
        }
    };

    let sources = sources_override.or(decision.sources.as_deref());
    let mut last_err = SearchExecError::Provider(format!("{provider}: all batch keys failed"));

    for lease in batch {
        let params = ProviderSearchParams {
            query: body.query.trim(),
            max_results,
            api_key: &lease.key,
            include_content,
            include_answer: true,
            search_depth: body.search_depth.as_deref(),
            tavily_topic: decision.tavily_topic.as_deref(),
            firecrawl_categories: decision.firecrawl_categories.as_deref(),
            sources,
            include_domains: if include_domains.is_empty() {
                None
            } else {
                Some(include_domains)
            },
            exclude_domains: if exclude_domains.is_empty() {
                None
            } else {
                Some(exclude_domains)
            },
            time_range: body.time_range.as_deref(),
            country: body.country.as_deref(),
            exact_match: body.exact_match,
        };

        match ctx.providers.search(provider, params, None).await {
            Ok(r) => {
                let _ = ctx.keys.report_success(lease.id).await;
                return Ok(r);
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if is_exhausted_status(provider, status) => {
                let _ = ctx.keys.report_exhausted(lease.id).await;
                last_err = SearchExecError::Provider(format!(
                    "{provider} exhausted status {status}: {b}"
                ));
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 429 || (500..600).contains(&status) => {
                // 429 only reaches here when not listed as exhausted for this provider
                let _ = ctx.keys.report_failure(lease.id).await;
                last_err =
                    SearchExecError::Provider(format!("{provider} upstream {status}: {b}"));
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) => {
                if status == 401 || status == 403 {
                    let _ = ctx.keys.report_failure(lease.id).await;
                    last_err =
                        SearchExecError::Provider(format!("{provider} upstream {status}: {b}"));
                    // try next key in batch
                    continue;
                }
                // non-retryable client error — stop batch
                return Err(SearchExecError::Provider(format!(
                    "{provider} upstream {status}: {b}"
                )));
            }
            Err(ProviderError::Http(e)) => {
                let _ = ctx.keys.report_failure(lease.id).await;
                last_err = SearchExecError::Search(format!("{provider} request failed: {e}"));
            }
        }
    }
    Err(last_err)
}

/// Public search used by HTTP handlers / MCP / research (auth already checked).
pub async fn search_inner(
    ctx: &ProductCtx,
    body: SearchQuery,
) -> Result<SearchResponse, SearchExecError> {
    if body.query.trim().is_empty() {
        return Err(SearchExecError::Search("missing_query".into()));
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

    let mut resp = if decision.hybrid {
        execute_hybrid(
            ctx,
            &body,
            &decision,
            max_results,
            include_content,
            &include_domains,
            &exclude_domains,
        )
        .await?
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
        .await?
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
        .await?
    };
    resp.route_debug = Some(RouteDebug {
        intent: Some(decision.intent.clone()),
        strategy: Some(decision.strategy.as_str().into()),
        reason: Some(decision.reason.clone()),
    });
    Ok(resp)
}

/// Mysearch `EXHAUSTED_STATUS` / `isExhaustedStatus` parity.
/// Credit/plan limits → `report_exhausted` (not consecutive fail).
pub fn is_exhausted_status(provider: &str, status: u16) -> bool {
    match provider {
        "tavily" => matches!(status, 429 | 432 | 433),
        "firecrawl" | "exa" => matches!(status, 402 | 429),
        "xai" => status == 429,
        _ => status == 402,
    }
}

#[cfg(test)]
mod exhausted_tests {
    use super::is_exhausted_status;

    #[test]
    fn tavily_plan_and_paygo() {
        assert!(is_exhausted_status("tavily", 429));
        assert!(is_exhausted_status("tavily", 432));
        assert!(is_exhausted_status("tavily", 433));
        assert!(!is_exhausted_status("tavily", 401));
    }

    #[test]
    fn firecrawl_exa_payment() {
        assert!(is_exhausted_status("firecrawl", 402));
        assert!(is_exhausted_status("exa", 402));
        assert!(is_exhausted_status("exa", 429));
    }

    #[test]
    fn xai_429() {
        assert!(is_exhausted_status("xai", 429));
        assert!(!is_exhausted_status("xai", 402));
    }

    #[test]
    fn unknown_provider_defaults_402() {
        assert!(is_exhausted_status("unknown", 402));
        assert!(!is_exhausted_status("unknown", 429));
    }
}
