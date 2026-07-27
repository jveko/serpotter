//! Single-URL extract chain (Firecrawl / Tavily).

use serpotter_keypool::KeyPoolError;
use serpotter_providers::{is_tunnel_error, ExtractResult, ProviderError, SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI};

use crate::dto::ExtractResponse;
use crate::error::ExtractError;
use crate::hold::{KeyHold, ProxyHold};
use crate::search::is_exhausted_status;
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
            // URL-class empty/failed extract: release holds, do not burn attempts or fail@3.
            // Outer extract_url chain continues to the next provider.
            Err(ProviderError::Unextractable { message, .. }) => {
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(ExtractError::Provider(format!(
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
                            let msg = crate::hold::truncate_err(&e.to_string());
                            h.finish_failure(Some(&msg)).await;
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
