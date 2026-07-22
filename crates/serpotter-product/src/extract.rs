//! Extract / research orchestration. No HTTP / auth.

use serpotter_core::{route_search, RouteInput, SearchQuery, Sources};
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{ExtractResult, ProviderError, SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI};

use crate::dto::{
    Citation, Evidence, ExtractResponse, ResearchRequest, ResearchResponse, ScrapedPage,
};
use crate::error::{ExtractError, ResearchError};
use crate::search::{is_exhausted_status, run_provider, search_inner};
use crate::ProductCtx;

pub async fn extract_url(
    ctx: &ProductCtx,
    url: &str,
    preferred: Option<&str>,
) -> Result<ExtractResponse, ExtractError> {
    let chain: Vec<&str> = match preferred {
        Some("tavily") => vec![SVC_TAVILY, SVC_FIRECRAWL],
        Some("firecrawl") | None => vec![SVC_FIRECRAWL, SVC_TAVILY],
        Some(other) => {
            return Err(ExtractError::Provider(format!(
                "unknown extract provider {other}"
            )));
        }
    };

    let mut last = ExtractError::NoHealthyKey("No healthy extract key".into());
    for provider in chain {
        match try_extract_provider(ctx, provider, url).await {
            Ok(r) => return Ok(to_response(r)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn try_extract_provider(
    ctx: &ProductCtx,
    provider: &str,
    url: &str,
) -> Result<ExtractResult, ExtractError> {
    let batch = match ctx.keys.acquire_batch(provider, 3).await {
        Ok(b) => b,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(ExtractError::NoHealthyKey(format!("No healthy {s} key")));
        }
        Err(KeyPoolError::Db(e)) => return Err(ExtractError::Db(e)),
    };

    let mut last = ExtractError::Provider(format!("{provider}: all keys failed"));
    for lease in batch {
        match ctx.providers.extract(provider, url, &lease.key).await {
            Ok(r) => {
                let _ = ctx.keys.report_success(lease.id).await;
                return Ok(r);
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if is_exhausted_status(provider, status) => {
                let _ = ctx.keys.report_exhausted(lease.id).await;
                last = ExtractError::Provider(format!(
                    "{provider} exhausted status {status}: {b}"
                ));
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 401
                || status == 403
                || status == 429
                || (500..600).contains(&status) =>
            {
                let _ = ctx.keys.report_failure(lease.id).await;
                last = ExtractError::Provider(format!("{provider} upstream {status}: {b}"));
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) => {
                return Err(ExtractError::Provider(format!(
                    "{provider} upstream {status}: {b}"
                )));
            }
            Err(ProviderError::Http(e)) => {
                let _ = ctx.keys.report_failure(lease.id).await;
                last = ExtractError::Provider(format!("{provider} request failed: {e}"));
            }
        }
    }
    Err(last)
}

fn to_response(r: ExtractResult) -> ExtractResponse {
    ExtractResponse {
        url: r.url,
        title: r.title,
        content: r.content,
        provider_used: r.provider,
    }
}

pub async fn research_inner(
    ctx: &ProductCtx,
    body: ResearchRequest,
) -> Result<ResearchResponse, ResearchError> {
    let max_results = body.web_max_results.unwrap_or(5).clamp(1, 20);
    // MCP default scrape_top_n=1; REST lean default 2
    let extract_n = body.scrape_top_n.unwrap_or(2).clamp(0, 10) as usize;
    let q = SearchQuery {
        query: body.query.clone(),
        max_results: Some(max_results),
        include_content: body.include_content.or(Some(false)),
        ..Default::default()
    };
    let search = search_inner(ctx, q)
        .await
        .map_err(ResearchError::Search)?;

    let mut scraped_pages = Vec::new();
    let mut citations = Vec::new();
    for item in &search.items {
        if !item.url.is_empty() {
            citations.push(Citation {
                title: item.title.clone(),
                url: item.url.clone(),
            });
        }
    }

    for item in search.items.iter().take(extract_n) {
        if item.url.is_empty() {
            continue;
        }
        match extract_url(ctx, &item.url, None).await {
            Ok(e) => {
                let excerpt = e.content.chars().take(280).collect::<String>();
                scraped_pages.push(ScrapedPage {
                    title: e.title,
                    url: e.url,
                    content: Some(e.content),
                    excerpt: Some(excerpt),
                    error: None,
                });
            }
            Err(err) => {
                scraped_pages.push(ScrapedPage {
                    title: Some(item.title.clone()),
                    url: item.url.clone(),
                    content: None,
                    excerpt: None,
                    error: Some(format!("{err:?}")),
                });
            }
        }
    }

    let providers_consulted = {
        let mut p = vec![search.provider_used.clone()];
        p.sort();
        p.dedup();
        p
    };

    let social_enabled = ctx.db.get_social_enabled().await.unwrap_or(true);
    let social_results = if body.social_max_results.unwrap_or(0) == 0 || !social_enabled {
        map_social_leg(body.social_max_results, social_enabled, None)
    } else {
        let n = body.social_max_results.unwrap_or(0).clamp(1, 10);
        let social_q = SearchQuery {
            query: body.query.clone(),
            max_results: Some(n),
            provider: Some(SVC_XAI.into()),
            sources: Some(Sources::One("x".into())),
            include_content: Some(false),
            ..Default::default()
        };
        let decision = route_search(RouteInput { query: &social_q });
        let x_sources = ["x".to_string()];
        let provider_result = match run_provider(
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
            Ok(r) => Ok(r.items),
            Err(_) => Err(()),
        };
        map_social_leg(Some(n), social_enabled, Some(provider_result))
    };

    Ok(ResearchResponse {
        query: body.query,
        web_results: search.items,
        social_results,
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
        }),
    })
}

/// Decide social leg outcome without I/O.
/// `provider_result`: Ok(items) / Err(()) from xAI attempt; ignored when leg skipped.
pub fn map_social_leg(
    social_max_results: Option<u32>,
    social_enabled: bool,
    provider_result: Option<Result<Vec<serpotter_core::SearchItem>, ()>>,
) -> Option<Vec<serpotter_core::SearchItem>> {
    let n = social_max_results.unwrap_or(0);
    if n == 0 || !social_enabled {
        return None; // skip leg
    }
    match provider_result {
        Some(Ok(items)) => Some(items),
        Some(Err(())) | None => Some(Vec::new()), // soft-empty
    }
}

#[cfg(test)]
mod social_leg_tests {
    use super::map_social_leg;

    #[test]
    fn skip_when_zero_or_disabled() {
        assert!(map_social_leg(None, true, Some(Ok(vec![]))).is_none());
        assert!(map_social_leg(Some(0), true, Some(Ok(vec![]))).is_none());
        assert!(map_social_leg(Some(3), false, Some(Ok(vec![]))).is_none());
    }

    #[test]
    fn soft_empty_on_provider_error() {
        let out = map_social_leg(Some(3), true, Some(Err(())));
        assert_eq!(out.as_ref().map(|v| v.len()), Some(0));
    }

    #[test]
    fn soft_empty_when_provider_not_run() {
        // defensive: enabled+n>0 but no result supplied
        let out = map_social_leg(Some(2), true, None);
        assert_eq!(out.as_ref().map(|v| v.len()), Some(0));
    }
}
