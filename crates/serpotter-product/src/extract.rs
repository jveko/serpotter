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
            Err(KeyPoolError::AcquireTimeout(s)) => {
                return Err(ExtractError::KeyBusy(format!(
                    "All {s} keys busy (acquire timeout)"
                )));
            }
            Err(KeyPoolError::Db(e)) => return Err(ExtractError::Db(e)),
        };
        let mut key_hold = KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);

        // Extract providers are web-only (no xAI), but keep the same skip rule.
        let proxy = if provider == SVC_XAI {
            None
        } else {
            match ctx.outbound.acquire().await {
                Ok(None) if ctx.outbound.require_proxy() => {
                    key_hold.finish_release().await;
                    return Err(ExtractError::NoHealthyNode(
                        "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)".into(),
                    ));
                }
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
    let search = search_inner(ctx, q)
        .await
        .map_err(ResearchError::Search)?;

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
                    Ok(e) => {
                        let provider = e.provider_used.clone();
                        let page = scraped_page_from_extract(
                            e.title,
                            e.url,
                            e.content,
                            include_scrape_content,
                        );
                        (page, Some(provider))
                    }
                    Err(err) => (
                        ScrapedPage {
                            title: Some(title),
                            url,
                            content: None,
                            excerpt: None,
                            error: Some(err.to_string()),
                        },
                        None,
                    ),
                }
            },
        ))
        .await;
        let mut pages = Vec::with_capacity(pairs.len());
        let mut scrape_providers = Vec::new();
        for (page, provider) in pairs {
            if let Some(p) = provider {
                scrape_providers.push(p);
            }
            pages.push(page);
        }
        (pages, scrape_providers)
    };

    let social_fut = async {
        if !run_social {
            (
                map_social_leg(body.social_max_results, social_enabled, None),
                None,
                false,
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
            let (provider_result, social_err, consulted) = match run_provider(
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
                Ok(r) => (Ok(r.items), None, true),
                Err(e) => (Err(()), Some(e.to_string()), false),
            };
            (
                map_social_leg(Some(n), social_enabled, Some(provider_result)),
                social_err,
                consulted,
            )
        }
    };

    let ((scraped_pages, scrape_providers), (social_results, social_error, social_consulted)) =
        tokio::join!(scrape_fut, social_fut);

    // Web primary first (request_log uses .first()); then xAI / scrape ids without re-sorting.
    let providers_consulted = merge_providers_consulted(
        search.provider_used.clone(),
        social_consulted.then(|| SVC_XAI.to_string()),
        scrape_providers,
    );

    Ok(ResearchResponse {
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
        }),
    })
}

/// Web provider stays first for request_log `.first()`; extras append unique only.
pub fn merge_providers_consulted(
    web: String,
    social: Option<String>,
    scrape_providers: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut out = vec![web];
    if let Some(s) = social {
        if !out.iter().any(|p| p == &s) {
            out.push(s);
        }
    }
    for sp in scrape_providers {
        if !out.iter().any(|p| p == &sp) {
            out.push(sp);
        }
    }
    out
}

/// Top N scrapable hits: non-empty URL first, then take(n).
pub fn select_scrape_targets(
    items: &[serpotter_core::SearchItem],
    extract_n: usize,
) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|item| !item.url.is_empty())
        .take(extract_n)
        .map(|item| (item.url.clone(), item.title.clone()))
        .collect()
}

/// Map extract success into ScrapedPage; full content only when include_content.
pub fn scraped_page_from_extract(
    title: Option<String>,
    url: String,
    content: String,
    include_content: bool,
) -> ScrapedPage {
    let excerpt = content.chars().take(280).collect::<String>();
    ScrapedPage {
        title,
        url,
        content: if include_content {
            Some(content)
        } else {
            None
        },
        excerpt: Some(excerpt),
        error: None,
    }
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

#[cfg(test)]
mod providers_consulted_tests {
    use super::merge_providers_consulted;

    #[test]
    fn web_stays_first_extras_unique() {
        let out = merge_providers_consulted(
            "tavily".into(),
            Some("xai".into()),
            vec!["firecrawl".into(), "tavily".into(), "firecrawl".into()],
        );
        assert_eq!(out, vec!["tavily", "xai", "firecrawl"]);
    }

    #[test]
    fn no_social_scrape_only() {
        let out = merge_providers_consulted("blend".into(), None, vec!["firecrawl".into()]);
        assert_eq!(out, vec!["blend", "firecrawl"]);
    }
}

#[cfg(test)]
mod scrape_mapper_tests {
    use super::{scraped_page_from_extract, select_scrape_targets};
    use serpotter_core::SearchItem;

    fn item(title: &str, url: &str) -> SearchItem {
        SearchItem {
            title: title.into(),
            url: url.into(),
            snippet: None,
            content: None,
            score: None,
            published: None,
            author: None,
            provider: None,
            source: None,
        }
    }

    #[test]
    fn select_filters_empty_before_take() {
        let items = vec![
            item("a", ""),
            item("b", "https://b.example"),
            item("c", "https://c.example"),
            item("d", "https://d.example"),
        ];
        let out = select_scrape_targets(&items, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "https://b.example");
        assert_eq!(out[1].0, "https://c.example");
    }

    #[test]
    fn select_take_zero() {
        let items = vec![item("a", "https://a.example")];
        assert!(select_scrape_targets(&items, 0).is_empty());
    }

    #[test]
    fn content_gated_off_keeps_excerpt() {
        let page = scraped_page_from_extract(
            Some("t".into()),
            "https://x".into(),
            "full body text here".into(),
            false,
        );
        assert!(page.content.is_none());
        assert_eq!(page.excerpt.as_deref(), Some("full body text here"));
        assert!(page.error.is_none());
    }

    #[test]
    fn content_gated_on_includes_full() {
        let page = scraped_page_from_extract(None, "https://x".into(), "BODY".into(), true);
        assert_eq!(page.content.as_deref(), Some("BODY"));
        assert_eq!(page.excerpt.as_deref(), Some("BODY"));
    }
}
