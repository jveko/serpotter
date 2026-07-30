//! Research orchestration: web search + scrape + optional social leg.

use serpotter_core::{route_search, RouteInput, SearchQuery, Sources};
use serpotter_providers::SVC_XAI;

use crate::dto::{Citation, Evidence, ResearchRequest, ResearchResponse, ScrapedPage};
use crate::error::ResearchError;
use crate::meta::{ExecMeta, ProductOutcome};
use crate::search::{run_provider, search_inner};
use crate::ProductCtx;

use super::extract_url::extract_url;
use super::helpers::{
    map_social_leg, merge_providers_consulted, scraped_page_from_extract, select_scrape_targets,
};

pub async fn research_inner(
    ctx: &ProductCtx,
    body: ResearchRequest,
) -> Result<ProductOutcome<ResearchResponse>, ProductOutcome<ResearchError>> {
    let max_results = body.web_max_results.unwrap_or(5).clamp(1, 20);
    // Default scrape_top_n=2 (REST + MCP); callers may set 0–10.
    let extract_n = body.scrape_top_n.unwrap_or(2).clamp(0, 10) as usize;
    // Web leg must NOT carry X handles — Gate 3 would steal routing to xAI.
    let q = SearchQuery {
        query: body.query.clone(),
        max_results: Some(max_results),
        include_content: body.include_content.or(Some(false)),
        include_domains: body.include_domains.clone(),
        exclude_domains: body.exclude_domains.clone(),
        from_date: body.from_date.clone(),
        to_date: body.to_date.clone(),
        time_range: body.time_range.clone(),
        country: body.country.clone(),
        ..Default::default()
    };
    let search_out = match search_inner(ctx, q).await {
        Ok(o) => o,
        Err(o) => {
            return Err(ProductOutcome {
                result: ResearchError::Search(o.result),
                meta: o.meta,
            });
        }
    };
    let mut meta = search_out.meta;
    let search = search_out.result;

    let mut citations = Vec::new();
    for item in &search.items {
        if !item.url.is_empty() {
            citations.push(Citation {
                title: item.title.clone(),
                url: item.url.clone(),
            });
        }
    }

    // Concurrent scrapes preserve input rank order via join_all.
    // Cap is extract_n ≤ 10; can thrash KEY_MAX_INFLIGHT when scrape_top_n > 3 — acceptable for personal-use.
    // Social does not depend on scrape results — overlap wall-clock with scrapes.
    let include_scrape_content = body.include_content.unwrap_or(false);
    let scrape_targets = select_scrape_targets(&search.items, extract_n);

    let social_enabled = ctx.db.get_social_enabled().await.unwrap_or(true);
    let social_n = body.social_max_results.unwrap_or(0);
    let run_social = social_n > 0 && social_enabled;

    let scrape_fut = async {
        let pairs = futures_util::future::join_all(scrape_targets.into_iter().map(
            |(url, title)| async move {
                match extract_url(ctx, &url, None).await {
                    Ok(o) => {
                        let e = o.result;
                        let provider = e.provider_used.clone();
                        let page = scraped_page_from_extract(
                            e.title,
                            e.url,
                            e.content,
                            include_scrape_content,
                        );
                        (page, Some(provider), o.meta)
                    }
                    Err(o) => (
                        ScrapedPage {
                            title: Some(title),
                            url,
                            content: None,
                            excerpt: None,
                            error: Some(o.result.to_string()),
                        },
                        None,
                        o.meta,
                    ),
                }
            },
        ))
        .await;
        let mut pages = Vec::with_capacity(pairs.len());
        let mut scrape_providers = Vec::new();
        let mut scrape_meta = ExecMeta::default();
        for (page, provider, m) in pairs {
            scrape_meta.absorb(m);
            if let Some(p) = provider {
                scrape_providers.push(p);
            }
            pages.push(page);
        }
        (pages, scrape_providers, scrape_meta)
    };

    let social_fut = async {
        if !run_social {
            (
                map_social_leg(body.social_max_results, social_enabled, None),
                None,
                false,
                ExecMeta::default(),
            )
        } else {
            let n = social_n.clamp(1, 10);
            // Social leg: handles + dates + relative time (not web domain filters).
            let social_q = SearchQuery {
                query: body.query.clone(),
                max_results: Some(n),
                provider: Some(SVC_XAI.into()),
                sources: Some(Sources::One("x".into())),
                include_content: Some(false),
                allowed_x_handles: body.allowed_x_handles.clone(),
                excluded_x_handles: body.excluded_x_handles.clone(),
                from_date: body.from_date.clone(),
                to_date: body.to_date.clone(),
                time_range: body.time_range.clone(),
                ..Default::default()
            };
            let decision = route_search(RouteInput { query: &social_q });
            let x_sources = ["x".to_string()];
            let (provider_result, social_err, consulted, social_meta) = match run_provider(
                ctx,
                SVC_XAI,
                &social_q,
                &decision,
                n,
                false,
                &[],
                &[],
                Some(x_sources.as_slice()),
            )
            .await
            {
                Ok(o) => (Ok(o.result.items), None, true, o.meta),
                Err(o) => (Err(()), Some(o.result.to_string()), false, o.meta),
            };
            (
                map_social_leg(Some(n), social_enabled, Some(provider_result)),
                social_err,
                consulted,
                social_meta,
            )
        }
    };

    let (
        (scraped_pages, scrape_providers, scrape_meta),
        (social_results, social_error, social_consulted, social_meta),
    ) = tokio::join!(scrape_fut, social_fut);
    meta.absorb(scrape_meta);
    meta.absorb(social_meta);

    // Web primary first (request_log uses .first()); then xAI / scrape ids without re-sorting.
    let providers_consulted = merge_providers_consulted(
        search.provider_used.clone(),
        social_consulted.then(|| SVC_XAI.to_string()),
        scrape_providers,
    );

    Ok(ProductOutcome {
        result: ResearchResponse {
            query: body.query,
            web_results: search.items,
            social_results,
            social_error,
            scraped_pages: if scraped_pages.is_empty() {
                None
            } else {
                Some(scraped_pages)
            },
            citations: if citations.is_empty() {
                None
            } else {
                Some(citations)
            },
            evidence: Some(Evidence {
                summary: search.answer,
                providers_consulted: Some(providers_consulted),
                web_leg_errors: search.leg_errors,
            }),
        },
        meta,
    })
}
