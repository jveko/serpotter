//! Search orchestration (multi-provider routing + RRF). No HTTP / auth.

use serpotter_core::{
    fallback_chain, reciprocal_rank_fusion, route_search, RouteDebug, RouteInput, RrfList,
    SearchQuery, SearchResponse, Strategy,
};
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{
    is_tunnel_error, ProviderError, ProviderResult, ProviderSearchParams, SVC_FIRECRAWL, SVC_TAVILY,
    SVC_XAI,
};

use crate::error::SearchExecError;
use crate::hold::{KeyHold, ProxyHold};
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

/// Run one provider: lease-one key (+ proxy unless xAI), dual-pool matrix, max 3 attempts.
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
    const MAX_ATTEMPTS: usize = 3;

    let sources = sources_override.or(decision.sources.as_deref());
    let allowed_handles = body
        .allowed_x_handles
        .as_ref()
        .map(|v| v.as_list())
        .filter(|v| !v.is_empty());
    let excluded_handles = body
        .excluded_x_handles
        .as_ref()
        .map(|v| v.as_list())
        .filter(|v| !v.is_empty());
    let mut last_err = SearchExecError::Provider(format!("{provider}: all attempts failed"));

    for _ in 0..MAX_ATTEMPTS {
        let lease = match ctx.keys.acquire(provider).await {
            Ok(k) => k,
            Err(KeyPoolError::NoHealthyKey(s)) => {
                return Err(SearchExecError::NoHealthyKey(format!(
                    "No healthy {s} key"
                )));
            }
            Err(KeyPoolError::AcquireTimeout(s)) => {
                return Err(SearchExecError::KeyBusy(format!(
                    "All {s} keys busy (acquire timeout)"
                )));
            }
            Err(KeyPoolError::Db(e)) => return Err(SearchExecError::Db(e)),
        };
        let mut key_hold = KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);

        // xAI never touches outbound; web providers acquire (Fixed / node / direct).
        let proxy = if provider == SVC_XAI {
            None
        } else {
            match ctx.outbound.acquire().await {
                Ok(p) => p,
                Err(serpotter_outbound::ProxyPoolError::Db(e)) => {
                    // Explicit release before return (Drop spawn is only the safety net).
                    key_hold.finish_release().await;
                    return Err(SearchExecError::Db(e));
                }
            }
        };
        let mut proxy_hold = proxy.as_ref().map(|p| {
            ProxyHold::new(std::sync::Arc::clone(&ctx.outbound), p.clone())
        });
        let proxy_url = proxy.as_ref().map(|p| p.url.as_str());

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
            allowed_x_handles: allowed_handles.as_deref(),
            excluded_x_handles: excluded_handles.as_deref(),
            from_date: body.from_date.as_deref(),
            to_date: body.to_date.as_deref(),
            time_range: body.time_range.as_deref(),
            country: body.country.as_deref(),
            exact_match: body.exact_match,
        };

        match ctx.providers.search(provider, params, proxy_url).await {
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
                last_err = SearchExecError::Provider(format!(
                    "{provider} exhausted status {status}: {b}"
                ));
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 401 || status == 403 => {
                key_hold.finish_failure().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last_err =
                    SearchExecError::Provider(format!("{provider} upstream {status}: {b}"));
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 429 || (500..600).contains(&status) => {
                // 429 only reaches here when not listed as exhausted for this provider
                key_hold.finish_failure().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last_err =
                    SearchExecError::Provider(format!("{provider} upstream {status}: {b}"));
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
                return Err(SearchExecError::Provider(format!(
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
                        // e.g. JSON decode after 2xx — do not fail@3 key or node
                        key_hold.finish_release().await;
                        if let Some(h) = proxy_hold.as_mut() {
                            h.finish_release().await;
                        }
                    }
                }
                last_err = SearchExecError::Search(format!("{provider} request failed: {e}"));
                continue;
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
