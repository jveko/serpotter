//! Single-provider attempt loop (key/proxy dual-pool matrix).

use serpotter_core::SearchQuery;
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{
    is_tunnel_error, ProviderError, ProviderResult, ProviderSearchParams, SVC_XAI,
};

use crate::error::SearchExecError;
use crate::hold::{KeyHold, ProxyHold};
use crate::ProductCtx;

use super::{is_exhausted_status, is_firecrawl_banned};

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

        // xAI never touches outbound; web providers acquire (node / direct).
        let proxy = if provider == SVC_XAI {
            None
        } else {
            match ctx.outbound.acquire().await {
                Ok(None) if ctx.outbound.require_proxy() => {
                    key_hold.finish_release().await;
                    return Err(SearchExecError::NoHealthyNode(
                        "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)".into(),
                    ));
                }
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
            // Search path should not see Unextractable; treat as non-retryable provider err.
            Err(ProviderError::Unextractable { message, .. }) => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(SearchExecError::Provider(format!(
                    "{provider} unextractable: {message}"
                )));
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
            }) if provider == "firecrawl" && is_firecrawl_banned(status, &b) => {
                tracing::warn!(
                    key_id = key_hold.key_id(),
                    status,
                    reason = "firecrawl_banned",
                    "firecrawl key banned; deleting from pool"
                );
                key_hold.finish_banned().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last_err =
                    SearchExecError::Provider(format!("{provider} banned status {status}: {b}"));
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
                            let msg = crate::hold::truncate_err(&e.to_string());
                            h.finish_failure(Some(&msg)).await;
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
