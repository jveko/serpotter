//! Recording-sink tests for search attempt/fallback emissions.
//! Providers point at 127.0.0.1:9 (connection refused → retryable failure).

use std::sync::{Arc, Mutex};

use serpotter_core::SearchQuery;
use serpotter_db::Db;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::{
    ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
};

use crate::meta::{ProgressEvent, ProgressSink};
use crate::{search_inner, ProductCtx};

/// Collects events in order for assertions.
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
    }
}

#[tokio::test]
async fn search_emits_attempt_retry_and_fallback_in_order() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-progress-test")
        .await
        .unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx(db, sink.clone());
    let body = SearchQuery {
        query: "hello".into(),
        max_results: Some(1),
        ..Default::default()
    };
    // Routes to tavily (single chain); connection refused → retryable failure.
    let _ = search_inner(&ctx, body).await;

    let events = sink.0.lock().unwrap().clone();

    // First provider: one Attempt per MAX_ATTEMPTS, numbered 1..=3.
    // (The chain then falls back to exa/firecrawl, which each emit their own
    // single Attempt before failing at the key pool — no key inserted for them.)
    let attempts: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
        .collect();
    assert_eq!(
        attempts.len(),
        3,
        "one Attempt per MAX_ATTEMPTS: {events:?}"
    );
    assert_eq!(
        attempts[0],
        &ProgressEvent::Attempt {
            service: "tavily".into(),
            attempt: 1,
            max: 3
        }
    );
    assert_eq!(
        attempts[2],
        &ProgressEvent::Attempt {
            service: "tavily".into(),
            attempt: 3,
            max: 3
        }
    );

    // Two Retry events after the first two failures, naming service + attempt.
    let retries: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
        .collect();
    assert_eq!(retries.len(), 2, "two retries after two failures: {events:?}");
    assert!(
        matches!(retries[0], ProgressEvent::Retry { service, attempt: 1, .. } if service == "tavily"),
        "retry names service and attempt: {events:?}"
    );

    // Order: interleaved attempts and retries before the chain falls back.
    assert!(
        matches!(&events[0], ProgressEvent::Attempt { service, attempt: 1, .. } if service == "tavily"),
        "first event is the first attempt: {events:?}"
    );
    assert!(
        matches!(&events[1], ProgressEvent::Retry { service, attempt: 1, .. } if service == "tavily"),
        "retry follows the failed attempt: {events:?}"
    );
    assert!(matches!(
        &events[2],
        ProgressEvent::Attempt { service, attempt: 2, .. } if service == "tavily"
    ));
    assert!(matches!(
        &events[3],
        ProgressEvent::Retry { service, attempt: 2, .. } if service == "tavily"
    ));
    assert!(matches!(
        &events[4],
        ProgressEvent::Attempt { service, attempt: 3, .. } if service == "tavily"
    ));

    // Fallback chain walked on total failure: tavily → exa → firecrawl.
    let fallbacks: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
        .collect();
    assert_eq!(
        fallbacks.len(),
        2,
        "two fallbacks for a 3-provider chain: {events:?}"
    );
    assert!(
        matches!(fallbacks[0], ProgressEvent::Fallback { from, to, .. } if from == "tavily" && to == "exa"),
        "fallback names the pair: {events:?}"
    );
    assert!(
        matches!(fallbacks[1], ProgressEvent::Fallback { from, to, .. } if from == "exa" && to == "firecrawl"),
        "fallback names the pair: {events:?}"
    );
}

#[tokio::test]
async fn extract_emits_attempt_retry_and_fallback_in_order() {
    let db = test_db().await;
    db.insert_api_key("firecrawl", "fc-progress-test")
        .await
        .unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx(db, sink.clone());
    // Preferred=firecrawl: chain is [firecrawl, tavily]; connection refused →
    // retryable Http failures, then the chain falls back to tavily (no key → NoHealthyKey).
    let _ = crate::extract_url(&ctx, "https://example.com", Some("firecrawl")).await;

    let events = sink.0.lock().unwrap().clone();

    // First provider: one Attempt per MAX_ATTEMPTS, numbered 1..=3.
    let attempts: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "firecrawl"))
        .collect();
    assert_eq!(
        attempts.len(),
        3,
        "one Attempt per MAX_ATTEMPTS: {events:?}"
    );
    assert_eq!(
        attempts[0],
        &ProgressEvent::Attempt {
            service: "firecrawl".into(),
            attempt: 1,
            max: 3
        }
    );
    assert_eq!(
        attempts[2],
        &ProgressEvent::Attempt {
            service: "firecrawl".into(),
            attempt: 3,
            max: 3
        }
    );

    // Two Retry events after the first two failures, naming service + attempt.
    let retries: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
        .collect();
    assert_eq!(retries.len(), 2, "two retries after two failures: {events:?}");
    assert!(
        matches!(retries[0], ProgressEvent::Retry { service, attempt: 1, .. } if service == "firecrawl"),
        "retry names service and attempt: {events:?}"
    );

    // Order: interleaved attempts and retries before the chain falls back.
    assert!(
        matches!(&events[0], ProgressEvent::Attempt { service, attempt: 1, .. } if service == "firecrawl"),
        "first event is the first attempt: {events:?}"
    );
    assert!(
        matches!(&events[1], ProgressEvent::Retry { service, attempt: 1, .. } if service == "firecrawl"),
        "retry follows the failed attempt: {events:?}"
    );
    assert!(matches!(
        &events[2],
        ProgressEvent::Attempt { service, attempt: 2, .. } if service == "firecrawl"
    ));
    assert!(matches!(
        &events[3],
        ProgressEvent::Retry { service, attempt: 2, .. } if service == "firecrawl"
    ));
    assert!(matches!(
        &events[4],
        ProgressEvent::Attempt { service, attempt: 3, .. } if service == "firecrawl"
    ));

    // Fallback chain walked on total failure: firecrawl → tavily.
    let fallbacks: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
        .collect();
    assert_eq!(
        fallbacks.len(),
        1,
        "one fallback for a 2-provider chain: {events:?}"
    );
    assert!(
        matches!(fallbacks[0], ProgressEvent::Fallback { from, to, .. } if from == "firecrawl" && to == "tavily"),
        "fallback names the pair: {events:?}"
    );
}

#[tokio::test]
async fn research_emits_web_phase_before_search_leg() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-progress-test")
        .await
        .unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx(db, sink.clone());
    let body = crate::dto::ResearchRequest {
        query: "hello".into(),
        web_max_results: Some(1),
        scrape_top_n: Some(0),
        social_max_results: Some(3),
        ..Default::default()
    };
    // Search leg fails (127.0.0.1:9): research short-circuits after the web phase,
    // so only the web Phase boundary is observable here (scrape/social need a live search).
    let _ = crate::research_inner(&ctx, body).await;

    let events = sink.0.lock().unwrap().clone();
    let phases: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Phase { .. }))
        .collect();
    assert_eq!(
        phases.len(),
        1,
        "web phase emitted before the search leg: {events:?}"
    );
    assert_eq!(
        phases[0],
        &ProgressEvent::Phase {
            name: "web".into(),
            done: 1,
            total: 3
        }
    );
}
