//! Strategy execute paths: single-chain, hybrid, blend.

use serpotter_core::{
    fallback_chain, reciprocal_rank_fusion, RrfList, SearchQuery, SearchResponse, Strategy,
};
use serpotter_providers::{SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI};

use crate::error::SearchExecError;
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
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
) -> Result<ProductOutcome<SearchResponse>, ProductOutcome<SearchExecError>> {
    let chain = fallback_chain(&decision.provider);
    let mut meta = ExecMeta::default();
    let mut last_err = SearchExecError::NoHealthyKey("No healthy provider key".into());

    for (i, provider) in chain.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: chain[i - 1].to_string(),
                to: provider.to_string(),
                reason: last_err.to_string(),
            });
        }
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
            Ok(o) => {
                meta.absorb(o.meta);
                return Ok(ProductOutcome {
                    result: o.result.into_search_response(),
                    meta,
                });
            }
            Err(o) => {
                meta.absorb(o.meta);
                last_err = o.result;
            }
        }
    }
    Err(ProductOutcome {
        result: last_err,
        meta,
    })
}

pub(super) async fn execute_hybrid(
    ctx: &ProductCtx,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<ProductOutcome<SearchResponse>, ProductOutcome<SearchExecError>> {
    let web_src = ["web".to_string()];
    let x_src = ["x".to_string()];
    let x_max = max_results.min(5);
    // Web leg uses the tavily fallback chain (tavily→exa→firecrawl), never
    // fallback_chain("hybrid") which would pull xAI into the web leg.
    let web_fut = async {
        let mut m = ExecMeta::default();
        let mut last = SearchExecError::NoHealthyKey("No healthy hybrid web key".into());
        let chain = fallback_chain("tavily");
        for (i, provider) in chain.iter().enumerate() {
            if i > 0 {
                ctx.emit(&ProgressEvent::Fallback {
                    from: chain[i - 1].to_string(),
                    to: provider.to_string(),
                    reason: last.to_string(),
                });
            }
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
                Ok(o) => {
                    m.absorb(o.meta);
                    return Ok(ProductOutcome {
                        result: o.result,
                        meta: m,
                    });
                }
                Err(o) => {
                    m.absorb(o.meta);
                    last = o.result;
                }
            }
        }
        Err(ProductOutcome {
            result: last,
            meta: m,
        })
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

    let mut meta = ExecMeta::default();
    match &web {
        Ok(o) => meta.absorb(o.meta.clone()),
        Err(o) => meta.absorb(o.meta.clone()),
    }
    match &x {
        Ok(o) => meta.absorb(o.meta.clone()),
        Err(o) => meta.absorb(o.meta.clone()),
    }

    let web_items = web
        .as_ref()
        .ok()
        .map(|o| o.result.items.as_slice())
        .unwrap_or(&[]);
    let x_items = x
        .as_ref()
        .ok()
        .map(|o| o.result.items.as_slice())
        .unwrap_or(&[]);
    if web_items.is_empty() && x_items.is_empty() {
        let err = match (web, x) {
            (Err(o), _) => o.result,
            (Ok(_), Err(o)) => o.result,
            _ => SearchExecError::Search("hybrid both legs empty".into()),
        };
        return Err(ProductOutcome { result: err, meta });
    }
    let leg_errors = multi_leg_errors([
        ("web", web.as_ref().err().map(|o| &o.result)),
        ("x", x.as_ref().err().map(|o| &o.result)),
    ]);
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
    let answer = web.as_ref().ok().and_then(|o| o.result.answer.clone());
    Ok(ProductOutcome {
        result: SearchResponse {
            query: body.query.clone(),
            provider_used: "hybrid".into(),
            items,
            answer,
            leg_errors,
            route_debug: None,
        },
        meta,
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
) -> Result<ProductOutcome<SearchResponse>, ProductOutcome<SearchExecError>> {
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

    let mut meta = ExecMeta::default();
    for leg in [&a, &b] {
        match leg {
            Ok(o) => meta.absorb(o.meta.clone()),
            Err(o) => meta.absorb(o.meta.clone()),
        }
    }
    if let Some(ref leg) = c {
        match leg {
            Ok(o) => meta.absorb(o.meta.clone()),
            Err(o) => meta.absorb(o.meta.clone()),
        }
    }

    let a_items = a
        .as_ref()
        .ok()
        .map(|o| o.result.items.as_slice())
        .unwrap_or(&[]);
    let b_items = b
        .as_ref()
        .ok()
        .map(|o| o.result.items.as_slice())
        .unwrap_or(&[]);
    let c_items = c
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|o| o.result.items.as_slice())
        .unwrap_or(&[]);

    if a_items.is_empty() && b_items.is_empty() && c_items.is_empty() {
        // Include Verify's third leg — dropping c.err() collapses KeyBusy/NoHealthy* into "blend empty".
        let err = first_blend_err(
            a.err().map(|o| o.result),
            b.err().map(|o| o.result),
            c.and_then(Result::err).map(|o| o.result),
        );
        return Err(ProductOutcome { result: err, meta });
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
        ("primary", a.as_ref().err().map(|o| &o.result)),
        ("secondary", b.as_ref().err().map(|o| &o.result)),
        (
            "exa",
            c.as_ref().and_then(|r| r.as_ref().err()).map(|o| &o.result),
        ),
    ]);
    let answer = a.as_ref().ok().and_then(|o| o.result.answer.clone());
    Ok(ProductOutcome {
        result: SearchResponse {
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
        },
        meta,
    })
}
