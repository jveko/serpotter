//! Strategy execute paths: single-chain, hybrid, blend.

use serpotter_core::{
    fallback_chain, reciprocal_rank_fusion, RrfList, SearchItem, SearchQuery, SearchResponse,
    Strategy,
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
        // The x (social) leg never carries the web-only domain filters: the
        // xAI social path cannot express them and would refuse the whole leg,
        // silently turning a hybrid request into web-only. Filters stay on the
        // web leg above.
        run_provider(
            ctx,
            SVC_XAI,
            body,
            decision,
            x_max,
            false,
            &[],
            &[],
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
    // F14: prefer the primary/web leg answer; fall back to the xAI leg summary
    // when the web leg failed or returned none — the x leg's parsed answer was
    // previously discarded even though x items appear in the merged result.
    let answer = web
        .as_ref()
        .ok()
        .and_then(|o| o.result.answer.clone())
        .or_else(|| x.as_ref().ok().and_then(|o| o.result.answer.clone()));
    Ok(ProductOutcome {
        result: SearchResponse {
            query: body.query.clone(),
            provider_used: "hybrid".into(),
            items,
            answer,
            leg_errors,
            route_debug: None,
            cache_hit: None,
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
    // F14: prefer the primary leg's answer; fall back to secondary, then the
    // Verify third leg — a failed primary must not discard the other legs'
    // synthesis when their items are in the merged result.
    let answer = a
        .as_ref()
        .ok()
        .and_then(|o| o.result.answer.clone())
        .or_else(|| b.as_ref().ok().and_then(|o| o.result.answer.clone()))
        .or_else(|| {
            c.as_ref()
                .and_then(|r| r.as_ref().ok())
                .and_then(|o| o.result.answer.clone())
        });
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
            cache_hit: None,
        },
        meta,
    })
}

/// B20/B29: Exa deep (embeddings-based) search leg — replaces the normal
/// single-chain execute when the client explicitly routes provider=exa with
/// `outputSchema`, a deep `search_depth` (deep-lite|deep|deep-reasoning), or
/// `strategy=deep`. The vendor does the embeddings-based rerank + optional
/// structured synthesis (`outputSchema` passthrough, B28) server-side; no
/// local embedding work exists (the design's B29 decision).
///
/// One attempt (no retry ladder): a deep-search call is a single atomic
/// vendor request; failures surface as provider errors. The synthesized
/// `output` (string or structured JSON) rides in `SearchResponse.answer`;
/// results keep title/url/content/score.
pub(super) async fn execute_deep_search(
    ctx: &ProductCtx,
    body: &SearchQuery,
    max_results: u32,
) -> Result<ProductOutcome<SearchResponse>, ProductOutcome<SearchExecError>> {
    use serpotter_keypool::KeyPoolError;
    use serpotter_providers::SVC_EXA;

    let mode = body
        .search_depth
        .as_deref()
        .filter(|d| serpotter_core::is_deep_mode(Some(d)))
        .unwrap_or("deep-lite");
    let mut meta = ExecMeta::default();
    ctx.emit(&ProgressEvent::Attempt {
        service: SVC_EXA.to_string(),
        attempt: 1,
        max: 1,
    });
    let lease = match ctx.keys.acquire(SVC_EXA).await {
        Ok(k) => k,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(ProductOutcome {
                result: SearchExecError::NoHealthyKey(format!("No healthy {s} key")),
                meta,
            });
        }
        Err(KeyPoolError::AcquireTimeout(s)) => {
            return Err(ProductOutcome {
                result: SearchExecError::KeyBusy(format!("All {s} keys busy (acquire timeout)")),
                meta,
            });
        }
        Err(KeyPoolError::Db(e)) => {
            return Err(ProductOutcome {
                result: SearchExecError::Db(e),
                meta,
            });
        }
    };
    let mut key_hold = crate::hold::KeyHold::new(std::sync::Arc::clone(&ctx.keys), lease.id);
    let proxy = match ctx.outbound.acquire().await {
        Ok(None) if ctx.outbound.require_proxy() => {
            key_hold.finish_release().await;
            return Err(ProductOutcome {
                result: SearchExecError::NoHealthyNode(
                    "No healthy outbound proxy node (REQUIRE_OUTBOUND_PROXY)".into(),
                ),
                meta,
            });
        }
        Ok(p) => p,
        Err(serpotter_outbound::ProxyPoolError::Db(e)) => {
            key_hold.finish_release().await;
            return Err(ProductOutcome {
                result: SearchExecError::Db(e),
                meta,
            });
        }
    };
    let mut proxy_hold = proxy
        .as_ref()
        .map(|p| crate::hold::ProxyHold::new(std::sync::Arc::clone(&ctx.outbound), p.clone()));
    let node_id = proxy_hold.as_ref().map(|h| h.node_id());
    let _key_id = key_hold.key_id();
    let proxy_url = proxy.as_ref().map(|p| p.url.as_str());
    let http = match ctx.providers.client_for(proxy_url) {
        Ok(h) => h,
        Err(e) => {
            key_hold.finish_release().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
            meta.note_attempt(SVC_EXA, lease.id, node_id, false);
            return Err(ProductOutcome {
                result: SearchExecError::Provider(format!("exa deep client: {e}")),
                meta,
            });
        }
    };
    let attempt = ctx
        .providers
        .exa
        .search_deep(
            &http,
            &lease.key,
            body.query.trim(),
            mode,
            Some(max_results),
            body.output_schema.as_ref(),
        )
        .await;
    match attempt {
        Ok(out) => {
            key_hold.finish_success().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_success().await;
            }
            meta.note_attempt(SVC_EXA, lease.id, node_id, true);
            // B2: Exa reports an exact costDollars figure — fold it in.
            meta.set_usage(None, None, None, out.cost);
            let items = out
                .items
                .into_iter()
                .map(|i| SearchItem {
                    title: i.title,
                    url: i.url,
                    snippet: None,
                    content: i.content,
                    score: i.score,
                    published: None,
                    author: None,
                    provider: Some(SVC_EXA.to_string()),
                    source: None,
                })
                .collect();
            let answer = out.output.map(|o| match o {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            });
            Ok(ProductOutcome {
                result: SearchResponse {
                    query: body.query.clone(),
                    provider_used: "exa-deep".into(),
                    items,
                    answer,
                    leg_errors: None,
                    route_debug: None,
                    cache_hit: None,
                },
                meta,
            })
        }
        Err(e) => {
            key_hold.finish_release().await;
            if let Some(h) = proxy_hold.as_mut() {
                h.finish_release().await;
            }
            meta.note_attempt(SVC_EXA, lease.id, node_id, false);
            Err(ProductOutcome {
                result: SearchExecError::Provider(match e {
                    serpotter_providers::ProviderError::Upstream { status, body, .. } => {
                        format!("exa deep upstream {status}: {body}")
                    }
                    serpotter_providers::ProviderError::Http(err) => {
                        format!("exa deep request failed: {err}")
                    }
                    serpotter_providers::ProviderError::Unsupported {
                        provider,
                        action,
                        detail,
                    } => format!("{provider} {action} unsupported: {detail}"),
                    other => format!("exa deep failed: {other}"),
                }),
                meta,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serpotter_core::{route_search, RouteInput, Sources, VecOrOne};
    use serpotter_db::Db;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient, SVC_XAI,
    };

    use crate::meta::{ProgressEvent, ProgressSink};
    use crate::ProductCtx;

    use super::*;

    #[derive(Clone, Default)]
    struct VecSink(Arc<Mutex<Vec<ProgressEvent>>>);

    impl ProgressSink for VecSink {
        fn emit(&self, event: &ProgressEvent) {
            self.0.lock().unwrap().push(event.clone());
        }
    }

    async fn test_db() -> Db {
        serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate")
    }

    fn test_ctx(db: Db, sink: VecSink) -> ProductCtx {
        let keys = Arc::new(KeyPool::new(db.clone()));
        let outbound = Arc::new(ProxyPool::new(db.clone()));
        let registry = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        ProductCtx {
            db,
            keys,
            outbound,
            providers: registry,
            progress: Some(Arc::new(sink)),
            request_timeout: std::time::Duration::from_secs(120),
            cache_enabled: true,
            cache_ttl: std::time::Duration::from_secs(300),
        }
    }

    /// Hybrid decision with web+x sources (what route_search produces for a
    /// hybrid body without an explicit provider).
    fn hybrid_decision(query: &SearchQuery) -> serpotter_core::RouteDecision {
        route_search(RouteInput { query })
    }

    /// Control: the x leg REFUSED locally when it still receives web-only
    /// domain filters — the xAI social path cannot express them, so the client
    /// errors before any network call (exactly one attempt, no retries). This
    /// is the discriminator that makes `hybrid_x_leg_strips_web_domain_filters`
    /// meaningful: stripping turns the refusal into a real 3-attempt loop.
    #[tokio::test]
    async fn xai_leg_with_web_domain_filters_refuses_immediately() {
        let db = test_db().await;
        db.insert_api_key("xai", "xai-domain-test").await.unwrap();
        let sink = VecSink::default();
        let ctx = test_ctx(db, sink.clone());
        let body = SearchQuery {
            query: "ai".into(),
            max_results: Some(3),
            ..Default::default()
        };
        let decision = hybrid_decision(&body);
        let domains = vec!["example.com".to_string()];
        let x_src = vec!["x".to_string()];
        let out = run_provider(
            &ctx,
            SVC_XAI,
            &body,
            &decision,
            3,
            false,
            &domains,
            &[],
            Some(x_src.as_slice()),
        )
        .await;
        let err = out.expect_err("domains on the social path must be refused");
        assert!(
            err.result.to_string().contains("unsupported"),
            "expected a local Unsupported refusal, got: {}",
            err.result
        );
        let events = sink.0.lock().unwrap().clone();
        let xai_attempts = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "xai"))
            .count();
        assert_eq!(
            xai_attempts, 1,
            "local refusal must stop after the first attempt: {events:?}"
        );
    }

    /// B7: hybrid must strip the web-only domain filters from the x leg. The
    /// x leg then runs its full retry loop against the unreachable upstream
    /// (3 attempts, 2 retries) instead of being refused before the network.
    #[tokio::test]
    async fn hybrid_x_leg_strips_web_domain_filters() {
        let db = test_db().await;
        db.insert_api_key("xai", "xai-hybrid-test").await.unwrap();
        db.insert_api_key("tavily", "tvly-hybrid-test")
            .await
            .unwrap();
        let sink = VecSink::default();
        let ctx = test_ctx(db, sink.clone());
        let body = SearchQuery {
            query: "ai".into(),
            sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
            include_domains: Some(VecOrOne::Many(vec!["example.com".into()])),
            max_results: Some(3),
            ..Default::default()
        };
        let decision = hybrid_decision(&body);
        assert!(decision.hybrid, "{decision:?}");
        let _ = execute_hybrid(
            &ctx,
            &body,
            &decision,
            3,
            false,
            &["example.com".to_string()],
            &[],
        )
        .await;

        let events = sink.0.lock().unwrap().clone();
        let xai_attempts = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "xai"))
            .count();
        assert_eq!(
            xai_attempts, 3,
            "x leg must attempt 3 times against :9, proving the domain filters were stripped: {events:?}"
        );
        let xai_retries = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Retry { service, .. } if service == "xai"))
            .count();
        assert_eq!(
            xai_retries, 2,
            "connection-refused retries, not a local refusal: {events:?}"
        );
    }
}
