//! Shared fallback-chain runner (execute_single_chain + hybrid web leg).

use serpotter_core::SearchQuery;
use serpotter_providers::ProviderResult;

use crate::error::SearchExecError;
use crate::meta::{ExecMeta, ProductOutcome, ProgressEvent};
use crate::ProductCtx;

use super::run_provider;

/// Try `providers` in order via one `run_provider` call per provider.
///
/// Emits `ProgressEvent::Fallback` on each hop (naming the pair + the failure
/// reason), absorbs every leg's meta, and returns the first `Ok` — or the last
/// leg's error. This is the exact loop `execute_single_chain` and the hybrid
/// web leg ran independently before the C1 unification.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_chain(
    ctx: &ProductCtx,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    providers: &[&str],
    sources_override: Option<&[String]>,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<ProductOutcome<ProviderResult>, ProductOutcome<SearchExecError>> {
    let mut meta = ExecMeta::default();
    let mut last_err = SearchExecError::NoHealthyKey("No healthy provider key".into());

    for (i, provider) in providers.iter().enumerate() {
        if i > 0 {
            ctx.emit(&ProgressEvent::Fallback {
                from: providers[i - 1].to_string(),
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
            sources_override,
        )
        .await
        {
            Ok(o) => {
                meta.absorb(o.meta);
                return Ok(ProductOutcome {
                    result: o.result,
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serpotter_core::{route_search, RouteInput, SearchQuery};
    use serpotter_db::Db;
    use serpotter_keypool::KeyPool;
    use serpotter_outbound::ProxyPool;
    use serpotter_providers::{
        ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient, SVC_XAI,
    };

    use crate::meta::{ProgressEvent, ProgressSink};
    use crate::search::run_provider;
    use crate::ProductCtx;

    use super::run_chain;

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

    fn single_decision(query: &SearchQuery) -> serpotter_core::RouteDecision {
        route_search(RouteInput { query })
    }

    /// Static-status mock upstream (empty JSON body) — progress_tests pattern.
    fn mock_upstream(status: u16) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).ok();
                    let resp = format!(
                        "HTTP/1.1 {status} Mock\r\ncontent-length: 0\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        format!("http://{addr}")
    }

    /// Sequenced mock: serves `seq[i]` on the i-th request (0-based), then keeps
    /// serving the last entry — lets tests prove "5xx, 5xx, 200 → success".
    fn mock_sequence(seq: Vec<(u16, &'static str)>) -> String {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("mock addr");
        let counter = Arc::new(AtomicUsize::new(0));
        let seq = Arc::new(seq);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let counter = Arc::clone(&counter);
                let seq = Arc::clone(&seq);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf).ok();
                    let idx = counter.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = seq[idx.min(seq.len() - 1)];
                    let resp = format!(
                        "HTTP/1.1 {status} Mock\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        format!("http://{addr}")
    }

    /// Valid Tavily `/search` 200 (the sequenced mock's success leg).
    const TAVILY_OK: &str = r#"{
  "query": "hello",
  "answer": "",
  "results": [
    {"title": "T1", "url": "https://t1.example/", "content": "t1 sufficiently long snippet body", "score": 0.9}
  ]
}"#;

    /// run_chain walks the provider list in order, emitting one Fallback per
    /// hop, and returns the last leg's error with all metas absorbed.
    #[tokio::test]
    async fn run_chain_walks_chain_and_emits_fallback_events() {
        let db = test_db().await;
        // Only tavily has a key: its :9 attempts exhaust the retry budget, then
        // the chain hops to exa/firecrawl which fail at the key pool.
        db.insert_api_key("tavily", "tvly-chain").await.unwrap();
        let sink = VecSink::default();
        let ctx = test_ctx(db, sink.clone());
        let body = SearchQuery {
            query: "hello".into(),
            max_results: Some(1),
            ..Default::default()
        };
        let decision = single_decision(&body);
        let chain: Vec<&str> = vec!["tavily", "exa", "firecrawl"];
        let out = run_chain(
            &ctx,
            &body,
            &decision,
            &chain,
            decision.sources.as_deref(),
            1,
            false,
            &[],
            &[],
        )
        .await;
        let err = out.expect_err("all three legs must fail");
        assert_eq!(
            err.result.to_string(),
            "No healthy firecrawl key",
            "last error must be the final leg's acquire failure, message passthrough (no double-wrap)"
        );

        let events = sink.0.lock().unwrap().clone();
        let fallbacks: Vec<&ProgressEvent> = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
            .collect();
        assert_eq!(
            fallbacks.len(),
            2,
            "two hops for a 3-provider chain: {events:?}"
        );
        assert!(
            matches!(&fallbacks[0], ProgressEvent::Fallback { from, to, .. } if from == "tavily" && to == "exa"),
            "first hop names the pair: {events:?}"
        );
        assert!(
            matches!(&fallbacks[1], ProgressEvent::Fallback { from, to, .. } if from == "exa" && to == "firecrawl"),
            "second hop names the pair: {events:?}"
        );
        // tavily ran its full 3-attempt budget (connection refused is retryable)
        // before the chain hopped.
        let tavily_attempts = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
            .count();
        assert_eq!(
            tavily_attempts, 3,
            "tavily attempts exhaust before the fallback: {events:?}"
        );
    }

    /// run_chain returns the first successful leg: a provider that returns
    /// 5xx twice then 200 must be retried (3 attempts, 2 Retry events), and
    /// the chain must NOT hop after the success.
    #[tokio::test]
    async fn run_chain_retries_5xx_twice_then_succeeds() {
        let db = test_db().await;
        db.insert_api_key("tavily", "tvly-chain-ok").await.unwrap();
        let sink = VecSink::default();
        let mut ctx = test_ctx(db, sink.clone());
        ctx.providers = ProviderRegistry::with_clients(
            TavilyClient::new(mock_sequence(vec![(500, ""), (500, ""), (200, TAVILY_OK)])),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let body = SearchQuery {
            query: "hello".into(),
            max_results: Some(1),
            ..Default::default()
        };
        let decision = single_decision(&body);
        let chain: Vec<&str> = vec!["tavily", "exa", "firecrawl"];
        let out = run_chain(
            &ctx,
            &body,
            &decision,
            &chain,
            decision.sources.as_deref(),
            1,
            false,
            &[],
            &[],
        )
        .await;
        let ok = out.expect("tavily succeeds on the third attempt");
        assert_eq!(ok.result.items.len(), 1, "the 200 leg's result is returned");

        let events = sink.0.lock().unwrap().clone();
        let attempts: Vec<&ProgressEvent> = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
            .collect();
        assert_eq!(
            attempts.len(),
            3,
            "5xx, 5xx, 200 = three attempts: {events:?}"
        );
        let retries = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Retry { service, .. } if service == "tavily"))
            .count();
        assert_eq!(retries, 2, "two Retry events after the two 5xx: {events:?}");
        let fallbacks = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
            .count();
        assert_eq!(fallbacks, 0, "success stops the chain: {events:?}");
        // Order: Attempt/Retry/Attempt/Retry/Attempt.
        assert!(
            matches!(&events[0], ProgressEvent::Attempt { service, attempt: 1, .. } if service == "tavily"),
            "first event is attempt 1: {events:?}"
        );
        assert!(
            matches!(&events[1], ProgressEvent::Retry { service, attempt: 1, .. } if service == "tavily"),
            "retry 1 follows attempt 1: {events:?}"
        );
        assert!(
            matches!(&events[2], ProgressEvent::Attempt { service, attempt: 2, .. } if service == "tavily"),
            "attempt 2 follows retry 1: {events:?}"
        );
        assert!(
            matches!(&events[3], ProgressEvent::Retry { service, attempt: 2, .. } if service == "tavily"),
            "retry 2 follows attempt 2: {events:?}"
        );
        assert!(
            matches!(&events[4], ProgressEvent::Attempt { service, attempt: 3, .. } if service == "tavily"),
            "attempt 3 follows retry 2: {events:?}"
        );
    }

    /// An exhausted status (tavily 429) exits run_provider immediately: one
    /// attempt, no Retry, and the message names the exhausted status.
    #[tokio::test]
    async fn run_provider_exhausted_returns_immediately() {
        let db = test_db().await;
        db.insert_api_key("tavily", "tvly-exh").await.unwrap();
        let sink = VecSink::default();
        let mut ctx = test_ctx(db, sink.clone());
        ctx.providers = ProviderRegistry::with_clients(
            TavilyClient::new(mock_upstream(429)),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let body = SearchQuery {
            query: "hello".into(),
            max_results: Some(1),
            ..Default::default()
        };
        let decision = single_decision(&body);
        let out = run_provider(&ctx, "tavily", &body, &decision, 1, false, &[], &[], None).await;
        let err = out.expect_err("exhausted must surface immediately");
        assert!(
            err.result.to_string().contains("exhausted status 429"),
            "message must name the exhausted status: {}",
            err.result
        );

        let events = sink.0.lock().unwrap().clone();
        let attempts = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
            .count();
        assert_eq!(
            attempts, 1,
            "no retry of the same exhausted account: {events:?}"
        );
        let retries = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
            .count();
        assert_eq!(retries, 0, "exhausted never emits Retry: {events:?}");
    }

    /// A local Unsupported refusal (domain filters on the xAI social path)
    /// returns immediately: one attempt, zero Retry events.
    #[tokio::test]
    async fn run_provider_unsupported_returns_immediately() {
        let db = test_db().await;
        db.insert_api_key("xai", "xai-unsup").await.unwrap();
        let sink = VecSink::default();
        let ctx = test_ctx(db, sink.clone());
        let body = SearchQuery {
            query: "ai".into(),
            max_results: Some(3),
            ..Default::default()
        };
        let decision = single_decision(&body);
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
        let err = out.expect_err("domain filter on the social path is refused locally");
        assert!(
            err.result.to_string().contains("unsupported"),
            "expected a local Unsupported refusal: {}",
            err.result
        );

        let events = sink.0.lock().unwrap().clone();
        let xai_attempts = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "xai"))
            .count();
        assert_eq!(
            xai_attempts, 1,
            "local refusal stops after the first attempt: {events:?}"
        );
        let retries = events
            .iter()
            .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
            .count();
        assert_eq!(retries, 0, "no retry for a local refusal: {events:?}");
    }
}
