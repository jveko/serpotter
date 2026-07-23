//! Extract / research orchestration. No HTTP / auth.

use serpotter_core::{route_search, RouteInput, SearchQuery, Sources};
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{
    is_tunnel_error, ExtractResult, ProviderError, SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI,
};

use crate::dto::{
    Citation, Evidence, ExtractResponse, ResearchRequest, ResearchResponse, ScrapedPage,
};
use crate::error::{ExtractError, ResearchError};
use crate::hold::{KeyHold, ProxyHold};
use crate::search::{is_exhausted_status, run_provider, search_inner};
use crate::ProductCtx;

pub async fn extract_url(
    ctx: &ProductCtx,
    url: &str,
    preferred: Option<&str>,
) -> Result<ExtractResponse, ExtractError> {
    let url = crate::ssrf::validate_extract_url(url)?;
    let url = url.as_str();
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
    const MAX_ATTEMPTS: usize = 3;

    let mut last = ExtractError::Provider(format!("{provider}: all attempts failed"));

    for _ in 0..MAX_ATTEMPTS {
        let lease = match ctx.keys.acquire(provider).await {
            Ok(k) => k,
            Err(KeyPoolError::NoHealthyKey(s)) => {
                return Err(ExtractError::NoHealthyKey(format!("No healthy {s} key")));
            }
            Err(KeyPoolError::Db(e)) => return Err(ExtractError::Db(e)),
        };
        let mut key_hold = KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);

        // Extract providers are web-only (no xAI), but keep the same skip rule.
        let proxy = if provider == SVC_XAI {
            None
        } else {
            match ctx.outbound.acquire().await {
                Ok(p) => p,
                Err(serpotter_outbound::ProxyPoolError::Db(e)) => {
                    key_hold.finish_release().await;
                    return Err(ExtractError::Db(e));
                }
            }
        };
        let mut proxy_hold = proxy.as_ref().map(|p| {
            ProxyHold::new(std::sync::Arc::clone(&ctx.outbound), p.clone())
        });
        let proxy_url = proxy.as_ref().map(|p| p.url.as_str());

        match ctx
            .providers
            .extract(provider, url, &lease.key, proxy_url)
            .await
        {
            Ok(r) => {
                key_hold.finish_success().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_success().await;
                }
                return Ok(r);
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if is_exhausted_status(provider, status) => {
                key_hold.finish_exhausted().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last = ExtractError::Provider(format!(
                    "{provider} exhausted status {status}: {b}"
                ));
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 401
                || status == 403
                || status == 429
                || (500..600).contains(&status) =>
            {
                key_hold.finish_failure().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last = ExtractError::Provider(format!("{provider} upstream {status}: {b}"));
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) => {
                // non-retryable: MUST report before return (no early-return leak)
                key_hold.finish_failure().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(ExtractError::Provider(format!(
                    "{provider} upstream {status}: {b}"
                )));
            }
            Err(ProviderError::Http(e)) => {
                match crate::classify_proxied_http(proxy.is_some(), is_tunnel_error(&e)) {
                    crate::ProxiedHttpClass::DirectKeyFailure => {
                        key_hold.finish_failure().await;
                    }
                    crate::ProxiedHttpClass::TunnelKeyReleaseNodeFailure => {
                        key_hold.finish_release().await;
                        if let Some(h) = proxy_hold.as_mut() {
                            h.finish_failure().await;
                        }
                    }
                    crate::ProxiedHttpClass::BothReleaseOnly => {
                        key_hold.finish_release().await;
                        if let Some(h) = proxy_hold.as_mut() {
                            h.finish_release().await;
                        }
                    }
                }
                last = ExtractError::Provider(format!("{provider} request failed: {e}"));
                continue;
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
        include_domains: body.include_domains.clone(),
        exclude_domains: body.exclude_domains.clone(),
        allowed_x_handles: body.allowed_x_handles.clone(),
        excluded_x_handles: body.excluded_x_handles.clone(),
        from_date: body.from_date.clone(),
        to_date: body.to_date.clone(),
        time_range: body.time_range.clone(),
        country: body.country.clone(),
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
        // Social leg: pass handles + dates only (not web domain filters).
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
