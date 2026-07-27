//! Strategy execute paths: single-chain, hybrid, blend.

use serpotter_core::{
    fallback_chain, reciprocal_rank_fusion, SearchQuery, SearchResponse, Strategy, RrfList,
};
use serpotter_providers::{SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI};

use crate::error::SearchExecError;
use crate::ProductCtx;

use super::run_provider;
use super::{first_blend_err, multi_leg_errors};

pub(super) async fn execute_single_chain(
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

pub(super) async fn execute_hybrid(
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
    let x_max = max_results.min(5);
    // Web leg uses the tavily fallback chain (tavily→exa→firecrawl), never
    // fallback_chain("hybrid") which would pull xAI into the web leg.
    let web_fut = async {
        let mut last = SearchExecError::NoHealthyKey("No healthy hybrid web key".into());
        for provider in fallback_chain("tavily") {
            match run_provider(
                ctx,
                provider,
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                Some(web_src.as_slice()),
            )
            .await
            {
                Ok(r) => return Ok(r),
                Err(e) => last = e,
            }
        }
        Err(last)
    };
    let (web, x) = tokio::join!(
        web_fut,
        run_provider(
            ctx,
            SVC_XAI,
            body,
            decision,
            x_max,
            false,
            include_domains,
            exclude_domains,
            Some(x_src.as_slice()),
        ),
    );

    let web_items = web.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let x_items = x.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    if web_items.is_empty() && x_items.is_empty() {
        return Err(web.err().or(x.err()).unwrap_or(SearchExecError::Search(
            "hybrid both legs empty".into(),
        )));
    }
    let leg_errors = multi_leg_errors([("web", web.as_ref().err()), ("x", x.as_ref().err())]);
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
        leg_errors,
        route_debug: None,
    })
}

pub(super) async fn execute_blend(
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

    // Independent provider legs run concurrently; soft-merge / RRF unchanged.
    let (a, b, c) = if decision.strategy == Strategy::Verify {
        let (a, b, c) = tokio::join!(
            run_provider(
                ctx,
                primary,
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                None,
            ),
            run_provider(
                ctx,
                secondary,
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                None,
            ),
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
            ),
        );
        (a, b, Some(c))
    } else {
        let (a, b) = tokio::join!(
            run_provider(
                ctx,
                primary,
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                None,
            ),
            run_provider(
                ctx,
                secondary,
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                None,
            ),
        );
        (a, b, None)
    };

    let a_items = a.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let b_items = b.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let c_items = c
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|r| r.items.as_slice())
        .unwrap_or(&[]);

    if a_items.is_empty() && b_items.is_empty() && c_items.is_empty() {
        // Include Verify's third leg — dropping c.err() collapses KeyBusy/NoHealthy* into "blend empty".
        return Err(first_blend_err(a.err(), b.err(), c.and_then(Result::err)));
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
    let leg_errors = multi_leg_errors([
        ("primary", a.as_ref().err()),
        ("secondary", b.as_ref().err()),
        ("exa", c.as_ref().and_then(|r| r.as_ref().err())),
    ]);
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
        leg_errors,
        route_debug: None,
    })
}
