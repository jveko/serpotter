//! Recording-sink tests for search attempt/fallback emissions.
//! Providers point at 127.0.0.1:9 (connection refused → retryable failure).

use std::sync::{Arc, Mutex};

use serpotter_core::SearchQuery;
use serpotter_db::Db;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::{ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient};

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

/// Spawn a blocking std-thread HTTP mock that answers every request with
/// `status` (empty JSON body, `connection: close`) and return its base URL.
/// Lets tests exercise upstream status handling (429 exhausted, 401, 500)
/// deterministically without network.
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
    assert_eq!(
        retries.len(),
        2,
        "two retries after two failures: {events:?}"
    );
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
    assert_eq!(
        retries.len(),
        2,
        "two retries after two failures: {events:?}"
    );
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

/// Build a ctx with the tavily client pointed at `tavily_url` (mock upstream),
/// the other providers at 127.0.0.1:9 (connection refused).
fn test_ctx_tavily_mock(db: Db, sink: VecSink, tavily_url: String) -> ProductCtx {
    let mut ctx = test_ctx(db, sink);
    ctx.providers = ProviderRegistry::with_clients(
        TavilyClient::new(tavily_url),
        FirecrawlClient::new("http://127.0.0.1:9"),
        ExaClient::new("http://127.0.0.1:9"),
        XaiClient::new("http://127.0.0.1:9"),
    );
    ctx
}

/// A4: an exhausted status (tavily 429) must NOT retry the same account — one
/// attempt, immediate Err, no Retry emit; the fallback reason names it honestly.
#[tokio::test]
async fn search_exhausted_returns_immediately_no_retry() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-exhausted").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx_tavily_mock(db.clone(), sink.clone(), mock_upstream(429));
    let body = SearchQuery {
        query: "hello".into(),
        max_results: Some(1),
        ..Default::default()
    };
    let _ = search_inner(&ctx, body).await;

    let events = sink.0.lock().unwrap().clone();
    let attempts: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
        .collect();
    assert_eq!(
        attempts.len(),
        1,
        "exhausted must not retry the same account 3×: {events:?}"
    );
    assert_eq!(
        attempts[0],
        &ProgressEvent::Attempt {
            service: "tavily".into(),
            attempt: 1,
            max: 3
        }
    );
    let retries: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
        .collect();
    assert_eq!(
        retries.len(),
        0,
        "no Retry emit for the exhausted path: {events:?}"
    );
    // The chain falls through to the next provider, reason = exhausted message.
    let fallbacks: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
        .collect();
    assert!(
        matches!(
            &fallbacks[0],
            ProgressEvent::Fallback { from, to, reason, .. }
                if from == "tavily" && to == "exa" && reason.contains("exhausted status 429")
        ),
        "fallback must carry the exhausted reason: {events:?}"
    );
    // Key stayed active and its NULL credits were not demoted (A3).
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1, "exhausted must not hard-disable the key");
}

/// A2: a 5xx upstream burst must NOT disable the key — every attempt releases
/// (finish_release), so consecutive_fails stays 0 and active stays 1.
#[tokio::test]
async fn search_upstream_5xx_releases_key_not_fails() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-5xx").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx_tavily_mock(db.clone(), sink.clone(), mock_upstream(500));
    let body = SearchQuery {
        query: "hello".into(),
        max_results: Some(1),
        ..Default::default()
    };
    let _ = search_inner(&ctx, body).await;

    // 3 retryable attempts then fallback — unchanged retry loop shape.
    let events = sink.0.lock().unwrap().clone();
    let attempts: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
        .collect();
    assert_eq!(attempts.len(), 3, "5xx is retryable: {events:?}");

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(
        row.consecutive_fails, 0,
        "5xx is transient vendor-side; must not fail@3 the key"
    );
    assert_eq!(row.active, 1, "key must stay enabled after a 5xx burst");
}

/// A2: 401/403 remains the auth-class signal that hard-disables a key.
#[tokio::test]
async fn search_upstream_401_disables_key() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-401").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx_tavily_mock(db.clone(), sink.clone(), mock_upstream(401));
    let body = SearchQuery {
        query: "hello".into(),
        max_results: Some(1),
        ..Default::default()
    };
    let _ = search_inner(&ctx, body).await;

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(
        row.consecutive_fails, 3,
        "401 is a dead-key signal: 3 attempts must count fails"
    );
    assert_eq!(
        row.active, 0,
        "401 must hard-disable the key (fail@3) — unlike 5xx/transport"
    );
}

/// A4 (extract): exhausted 429 returns immediately with no Retry.
#[tokio::test]
async fn extract_exhausted_returns_immediately_no_retry() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-ext-exhausted")
        .await
        .unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx_tavily_mock(db, sink.clone(), mock_upstream(429));
    // Preferred=tavily: chain is [tavily, firecrawl].
    let _ = crate::extract_url(&ctx, "https://example.com", Some("tavily")).await;

    let events = sink.0.lock().unwrap().clone();
    let attempts: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
        .collect();
    assert_eq!(
        attempts.len(),
        1,
        "exhausted extract must not retry the same account: {events:?}"
    );
    let retries: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Retry { .. }))
        .collect();
    assert_eq!(
        retries.len(),
        0,
        "no Retry emit for the exhausted extract path: {events:?}"
    );
    let fallbacks: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Fallback { .. }))
        .collect();
    assert!(
        matches!(
            &fallbacks[0],
            ProgressEvent::Fallback { from, to, reason, .. }
                if from == "tavily" && to == "firecrawl" && reason.contains("exhausted status 429")
        ),
        "fallback must carry the exhausted reason: {events:?}"
    );
}

/// A2 (extract): 5xx releases the key; 401 still disables it.
#[tokio::test]
async fn extract_upstream_5xx_releases_key_not_fails() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-ext-5xx").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx_tavily_mock(db.clone(), sink.clone(), mock_upstream(500));
    let _ = crate::extract_url(&ctx, "https://example.com", Some("tavily")).await;

    let events = sink.0.lock().unwrap().clone();
    let attempts: Vec<&ProgressEvent> = events
        .iter()
        .filter(|e| matches!(e, ProgressEvent::Attempt { service, .. } if service == "tavily"))
        .collect();
    assert_eq!(attempts.len(), 3, "5xx is retryable in extract: {events:?}");

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(
        row.consecutive_fails, 0,
        "extract 5xx must not fail@3 the key"
    );
    assert_eq!(row.active, 1);
}

#[tokio::test]
async fn extract_upstream_401_disables_key() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-ext-401").await.unwrap();
    let sink = VecSink::default();
    let ctx = test_ctx_tavily_mock(db.clone(), sink.clone(), mock_upstream(401));
    let _ = crate::extract_url(&ctx, "https://example.com", Some("tavily")).await;

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.consecutive_fails, 3, "extract 401 must count fails");
    assert_eq!(row.active, 0, "extract 401 must hard-disable the key");
}
