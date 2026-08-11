//! Single-URL extract chain (Firecrawl / Tavily).

use serpotter_keypool::KeyPoolError;
use serpotter_providers::{
    is_tunnel_error, ExtractResult, ProviderError, SVC_FIRECRAWL, SVC_TAVILY, SVC_XAI,
};

use crate::dto::ExtractResponse;
use crate::error::ExtractError;
use crate::hold::{KeyHold, ProxyHold};
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::search::{is_exhausted_status, is_firecrawl_banned};
use crate::ProductCtx;

pub async fn extract_url(
    ctx: &ProductCtx,
    url: &str,
    preferred: Option<&str>,
) -> Result<ProductOutcome<ExtractResponse>, ProductOutcome<ExtractError>> {
    let url = match crate::ssrf::validate_extract_url(url) {
        Ok(u) => u,
        Err(e) => {
            return Err(ProductOutcome {
                result: e,
                meta: ExecMeta::default(),
            });
        }
    };
    let url = url.as_str();
    let chain: Vec<&str> = match preferred {
        Some("tavily") => vec![SVC_TAVILY, SVC_FIRECRAWL],
        Some("firecrawl") | None => vec![SVC_FIRECRAWL, SVC_TAVILY],
        Some(other) => {
            return Err(ProductOutcome {
                result: ExtractError::Provider(format!("unknown extract provider {other}")),
                meta: ExecMeta::default(),
            });
        }
    };

    let mut meta = ExecMeta::default();
    let mut last = ExtractError::NoHealthyKey("No healthy extract key".into());
    for (i, provider) in chain.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: chain[i - 1].to_string(),
                to: provider.to_string(),
                reason: last.to_string(),
            });
        }
        match try_extract_provider(ctx, provider, url).await {
            Ok(o) => {
                meta.absorb(o.meta);
                return Ok(ProductOutcome {
                    result: to_response(o.result),
                    meta,
                });
            }
            Err(o) => {
                meta.absorb(o.meta);
                last = o.result;
            }
        }
    }
    Err(ProductOutcome { result: last, meta })
}

async fn try_extract_provider(
    ctx: &ProductCtx,
    provider: &str,
    url: &str,
) -> Result<ProductOutcome<ExtractResult>, ProductOutcome<ExtractError>> {
    const MAX_ATTEMPTS: usize = 3;

    let mut meta = ExecMeta::default();
    let mut last = ExtractError::Provider(format!("{provider}: all attempts failed"));

    for (attempt_idx, _) in (0..MAX_ATTEMPTS).enumerate() {
        ctx.emit(&ProgressEvent::Attempt {
            service: provider.to_string(),
            attempt: attempt_idx as u32 + 1,
            max: MAX_ATTEMPTS as u32,
        });
        let lease = match ctx.keys.acquire(provider).await {
            Ok(k) => k,
            Err(KeyPoolError::NoHealthyKey(s)) => {
                return Err(ProductOutcome {
                    result: ExtractError::NoHealthyKey(format!("No healthy {s} key")),
                    meta,
                });
            }
            Err(KeyPoolError::AcquireTimeout(s)) => {
                return Err(ProductOutcome {
                    result: ExtractError::KeyBusy(format!("All {s} keys busy (acquire timeout)")),
                    meta,
                });
            }
            Err(KeyPoolError::Db(e)) => {
                return Err(ProductOutcome {
                    result: ExtractError::Db(e),
                    meta,
                });
            }
        };
        let mut key_hold = KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);

        // Extract providers are web-only (no xAI), but keep the same skip rule.
        let proxy = if provider == SVC_XAI {
            None
        } else {
            match ctx.outbound.acquire().await {
                Ok(None) if ctx.outbound.require_proxy() => {
                    key_hold.finish_release().await;
                    return Err(ProductOutcome {
                        result: ExtractError::NoHealthyNode(
                            "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)".into(),
                        ),
                        meta,
                    });
                }
                Ok(p) => p,
                Err(serpotter_outbound::ProxyPoolError::Db(e)) => {
                    key_hold.finish_release().await;
                    return Err(ProductOutcome {
                        result: ExtractError::Db(e),
                        meta,
                    });
                }
            }
        };
        let mut proxy_hold = proxy
            .as_ref()
            .map(|p| ProxyHold::new(std::sync::Arc::clone(&ctx.outbound), p.clone()));
        let node_id = proxy_hold.as_ref().map(|h| h.node_id());
        let key_id = key_hold.key_id();
        let proxy_url = proxy.as_ref().map(|p| p.url.as_str());

        let span = tracing::info_span!(
            "provider_attempt",
            service = provider,
            key_id = key_id,
            node_id = ?node_id,
            attempt = attempt_idx,
            outcome = tracing::field::Empty,
        );
        let _guard = span.enter();

        let attempt = ctx
            .providers
            .extract(provider, url, &lease.key, proxy_url)
            .await;
        span.record(
            "outcome",
            match &attempt {
                Ok(_) => "ok",
                Err(ProviderError::Upstream { status, .. })
                    if is_exhausted_status(provider, *status) =>
                {
                    "exhausted"
                }
                Err(ProviderError::Http(e)) if is_tunnel_error(e) => "timeout",
                Err(_) => "error",
            },
        );
        match attempt {
            Ok(r) => {
                meta.note_attempt(provider, key_id, node_id, true);
                key_hold.finish_success().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_success().await;
                }
                return Ok(ProductOutcome { result: r, meta });
            }
            // URL-class empty/failed extract: release holds, do not burn attempts or fail@3.
            // Outer extract_url chain continues to the next provider.
            Err(ProviderError::Unextractable { message, .. }) => {
                meta.note_attempt(provider, key_id, node_id, false);
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(ProductOutcome {
                    result: ExtractError::Provider(format!("{provider} unextractable: {message}")),
                    meta,
                });
            }
            // Local dispatch failure (provider does not support extract): release
            // holds so the outer chain can try the next provider. Never an
            // upstream status.
            Err(ProviderError::Unsupported {
                provider,
                action,
                detail,
            }) => {
                meta.note_attempt(&provider, key_id, node_id, false);
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(ProductOutcome {
                    result: ExtractError::Provider(format!(
                        "{provider} {action} unsupported: {detail}"
                    )),
                    meta,
                });
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if is_exhausted_status(provider, status) => {
                // Exhausted = plan/credit limit. Report once and surface
                // immediately: retrying the same account 3× against the same
                // limit is pure waste. The outer extract chain falls through to
                // the next provider on Err, and the next request acquires a
                // fresh (non-exhausted) key from the pool.
                meta.note_attempt(provider, key_id, node_id, false);
                key_hold.finish_exhausted().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(ProductOutcome {
                    result: ExtractError::Provider(format!(
                        "{provider} exhausted status {status}: {b}"
                    )),
                    meta,
                });
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
                meta.note_attempt(provider, key_id, node_id, false);
                key_hold.finish_banned().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last = ExtractError::Provider(format!("{provider} banned status {status}: {b}"));
                if attempt_idx + 1 < MAX_ATTEMPTS {
                    ctx.emit(&ProgressEvent::Retry {
                        service: provider.to_string(),
                        attempt: attempt_idx as u32 + 1,
                        reason: last.to_string(),
                    });
                }
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 401 || status == 403 => {
                // Auth-class failure (invalid/revoked key) is the ONLY signal
                // that hard-disables a key (fail@3 → active=0 for 24h).
                meta.note_attempt(provider, key_id, node_id, false);
                key_hold.finish_failure().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last = ExtractError::Provider(format!("{provider} upstream {status}: {b}"));
                if attempt_idx + 1 < MAX_ATTEMPTS {
                    ctx.emit(&ProgressEvent::Retry {
                        service: provider.to_string(),
                        attempt: attempt_idx as u32 + 1,
                        reason: last.to_string(),
                    });
                }
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 429 || (500..600).contains(&status) => {
                // 429/5xx are transient vendor-side conditions: release the
                // key, never hard-disable it. Dead keys are caught by 401/403.
                meta.note_attempt(provider, key_id, node_id, false);
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                last = ExtractError::Provider(format!("{provider} upstream {status}: {b}"));
                if attempt_idx + 1 < MAX_ATTEMPTS {
                    ctx.emit(&ProgressEvent::Retry {
                        service: provider.to_string(),
                        attempt: attempt_idx as u32 + 1,
                        reason: last.to_string(),
                    });
                }
                continue;
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) => {
                // non-retryable 4xx (e.g. 400): the request is invalid, not the
                // key — release without fail@3 (MUST report before return, no
                // early-return leak).
                meta.note_attempt(provider, key_id, node_id, false);
                key_hold.finish_release().await;
                if let Some(h) = proxy_hold.as_mut() {
                    h.finish_release().await;
                }
                return Err(ProductOutcome {
                    result: ExtractError::Provider(format!("{provider} upstream {status}: {b}")),
                    meta,
                });
            }
            Err(ProviderError::Http(e)) => {
                meta.note_attempt(provider, key_id, node_id, false);
                match crate::classify_proxied_http(proxy.is_some(), is_tunnel_error(&e)) {
                    crate::ProxiedHttpClass::DirectKeyFailure => {
                        // Transport error without a proxy lease: release only.
                        // Transport never hard-disables a key — dead keys are
                        // caught by 401/403 upstream responses.
                        key_hold.finish_release().await;
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
                if attempt_idx + 1 < MAX_ATTEMPTS {
                    ctx.emit(&ProgressEvent::Retry {
                        service: provider.to_string(),
                        attempt: attempt_idx as u32 + 1,
                        reason: last.to_string(),
                    });
                }
                continue;
            }
        }
    }
    Err(ProductOutcome { result: last, meta })
}

fn to_response(r: ExtractResult) -> ExtractResponse {
    ExtractResponse {
        url: r.url,
        title: r.title,
        content: r.content,
        provider_used: r.provider,
    }
}
