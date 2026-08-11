//! Happy-path tests for blend/hybrid/research (F17).
//!
//! Uses a loopback `std::thread` TcpListener mock that serves canned per-route
//! 200 JSON (Tavily/Firecrawl/Exa search shapes, Firecrawl scrape, xAI
//! `/responses`), routing by request path + body marker. Every provider client
//! points at the SAME mock URL; reqwest sends `connection: close` so each
//! attempt opens a fresh connection the listener loop can serve concurrently.

use std::sync::{Arc, Mutex};

use serpotter_core::{SearchQuery, Sources};
use serpotter_db::Db;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::{ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient};

use crate::dto::ResearchRequest;
use crate::meta::{ProgressEvent, ProgressSink};
use crate::{research_inner, search_inner, ProductCtx};

// --- canned wire shapes ------------------------------------------------------

/// Tavily `/search` 200: 2 web results, NO answer (so D3's x-leg answer
/// fallback is observable).
const TAVILY_OK_NO_ANSWER: &str = r#"{
  "query": "hello",
  "answer": "",
  "results": [
    {"title": "T1", "url": "https://t1.example/", "content": "t1 sufficiently long snippet body", "score": 0.9},
    {"title": "T2", "url": "https://t2.example/", "content": "t2 sufficiently long snippet body", "score": 0.8}
  ]
}"#;

/// Tavily `/search` 200: 3 results (scrape targets for the research test).
const TAVILY_OK_THREE: &str = r#"{
  "query": "hello",
  "answer": "",
  "results": [
    {"title": "R1", "url": "https://r1.example/", "content": "r1 sufficiently long snippet body", "score": 0.9},
    {"title": "R2", "url": "https://r2.example/", "content": "r2 sufficiently long snippet body", "score": 0.8},
    {"title": "R3", "url": "https://r3.example/", "content": "r3 sufficiently long snippet body", "score": 0.7}
  ]
}"#;

/// Firecrawl `/v2/search` 200: one web result.
const FIRECRAWL_SEARCH_OK: &str = r#"{
  "data": {"web": [{"title": "F1", "url": "https://f1.example/", "description": "f1 sufficiently long snippet body"}]}
}"#;

/// Firecrawl `/v2/scrape` 200: one markdown page (B21 v2 shape).
const FIRECRAWL_SCRAPE_OK: &str = r#"{
  "data": {"markdown": "scraped page content for s1", "metadata": {"title": "S1", "sourceURL": "https://s1.example/"}}
}"#;

/// Firecrawl `/v2/extract` 200: job started (B18).
const FIRECRAWL_EXTRACT_START: &str = r#"{"success":true,"id":"job-1"}"#;

/// Firecrawl `/v2/extract/{id}` 200: job completed with the canned JSON.
const FIRECRAWL_EXTRACT_COMPLETED: &str =
    r#"{"success":true,"status":"completed","data":{"company":{"name":"Acme Corp"}}}"#;

/// xAI `/responses` 200: output_text summary + one url_citation.
const XAI_OK: &str = r#"{
  "output_text": "The xAI summary answer",
  "citations": [{"title": "X1", "url": "https://x1.example/"}]
}"#;

// --- loopback mock -----------------------------------------------------------

#[derive(Clone)]
struct MockRoute {
    /// Exact request path (e.g. "/search").
    path: &'static str,
    /// Substring that must appear in the JSON body (disambiguates tavily vs
    /// exa, which share the /search path).
    body_marker: Option<&'static str>,
    status: u16,
    body: &'static str,
}

fn find_route<'a>(routes: &'a [MockRoute], path: &str, body: &str) -> Option<&'a MockRoute> {
    routes
        .iter()
        .find(|r| r.path == path && r.body_marker.is_none_or(|m| body.contains(m)))
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read one HTTP request (head + body) from the stream. Returns None on a
/// closed/parse-failed connection.
fn read_http_request(stream: &mut std::net::TcpStream) -> Option<(String, String)> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let mut content_length: Option<usize> = None;
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if content_length.is_none() {
            if let Some(pos) = find_bytes(&buf, b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                content_length = head.lines().find_map(|l| {
                    let lower = l.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                });
                // No content-length (and no body expected, e.g. a status GET):
                // the head terminator ends the request — do not block waiting
                // for a body the client never sends.
                if content_length.is_none() {
                    break;
                }
            }
        }
        if let Some(cl) = content_length {
            let head_end = find_bytes(&buf, b"\r\n\r\n").map(|p| p + 4).unwrap_or(0);
            if buf.len() >= head_end.saturating_add(cl) {
                break;
            }
        }
    }
    let head_end = find_bytes(&buf, b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let body = String::from_utf8_lossy(&buf[head_end..]).to_string();
    Some((head, body))
}

/// Spawn the mock and return its base URL. Unknown routes answer 500 with a
/// marker body so a test that relies on an unmocked leg fails loudly (as an
/// upstream error) instead of hanging.
fn spawn_mock(routes: Vec<MockRoute>) -> String {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let addr = listener.local_addr().expect("mock addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let routes = routes.clone();
            std::thread::spawn(move || {
                let (head, body) = match read_http_request(&mut stream) {
                    Some(v) => v,
                    None => return,
                };
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let route = find_route(&routes, &path, &body)
                    .cloned()
                    .unwrap_or(MockRoute {
                        path: "__fallback__",
                        body_marker: None,
                        status: 500,
                        body: r#"{"error":"no mock route for this request"}"#,
                    });
                let resp = format!(
                    "HTTP/1.1 {} Mock\r\ncontent-length: {}\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{}",
                    route.status,
                    route.body.len(),
                    route.body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            });
        }
    });
    format!("http://{addr}")
}

// --- fixture -----------------------------------------------------------------

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

fn test_ctx_mock(db: Db, mock_url: String, sink: VecSink) -> ProductCtx {
    let keys = Arc::new(KeyPool::new(db.clone()));
    let outbound = Arc::new(ProxyPool::new(db.clone()));
    let registry = ProviderRegistry::with_clients(
        TavilyClient::new(mock_url.clone()),
        FirecrawlClient::new(mock_url.clone()),
        ExaClient::new(mock_url.clone()),
        XaiClient::new(mock_url),
    );
    ProductCtx {
        db,
        keys,
        outbound,
        providers: registry,
        progress: Some(Arc::new(sink)),
        request_timeout: std::time::Duration::from_secs(120),
    }
}

// --- (a) single-provider success path ----------------------------------------

#[tokio::test]
async fn single_provider_success_path() {
    let db = test_db().await;
    let key = db
        .insert_api_key("tavily", "tvly-happy-single")
        .await
        .unwrap();
    let mock = spawn_mock(vec![MockRoute {
        path: "/search",
        body_marker: Some("\"topic\""),
        status: 200,
        body: TAVILY_OK_NO_ANSWER,
    }]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let body = SearchQuery {
        query: "hello".into(),
        provider: Some("tavily".into()),
        max_results: Some(5),
        ..Default::default()
    };
    let out = search_inner(&ctx, body).await.expect("single success");
    assert_eq!(out.result.provider_used, "tavily");
    assert_eq!(out.result.items.len(), 2, "both tavily results survive");
    assert_eq!(out.result.answer, None, "no web answer in this fixture");
    // meta: one successful attempt, sticky key, first-seen consulted, raw strategy.
    assert_eq!(out.meta.attempt_count, 1);
    assert_eq!(out.meta.providers_consulted, vec!["tavily"]);
    assert_eq!(out.meta.key_id, Some(key.id));
    assert!(out.meta.node_id.is_none(), "no proxy nodes in test ctx");
    assert_eq!(out.meta.strategy.as_deref(), Some("fast"));
    // route_debug still reports the raw strategy + intent.
    let debug = out.result.route_debug.expect("route debug");
    assert_eq!(debug.strategy.as_deref(), Some("fast"));
    assert_eq!(debug.intent.as_deref(), Some("factual"));
}

// --- (b) BLEND: primary leg fails, chain falls through -----------------------

/// Balanced blend: the primary (tavily) leg fails with a retryable 500; the
/// secondary (firecrawl) succeeds — results survive and the failure is
/// reported honestly instead of being silently dropped.
#[tokio::test]
async fn balanced_blend_falls_through_failed_primary() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-happy-blend")
        .await
        .unwrap();
    let fc_key = db
        .insert_api_key("firecrawl", "fc-happy-blend")
        .await
        .unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/search",
            body_marker: Some("\"topic\""),
            status: 500,
            body: r#"{"error":"mock tavily boom"}"#,
        },
        MockRoute {
            path: "/v2/search",
            body_marker: None,
            status: 200,
            body: FIRECRAWL_SEARCH_OK,
        },
    ]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let body = SearchQuery {
        query: "hello".into(),
        strategy: Some("balanced".into()),
        max_results: Some(5),
        ..Default::default()
    };
    let out = search_inner(&ctx, body)
        .await
        .expect("blend keeps secondary success");
    assert_eq!(out.result.provider_used, "blend");
    assert!(!out.result.items.is_empty(), "firecrawl items must survive");
    let leg_errors = out.result.leg_errors.as_ref().expect("failed leg reported");
    assert!(
        leg_errors.iter().any(|e| e.contains("tavily")),
        "primary failure listed: {leg_errors:?}"
    );
    // first-seen attempted providers — primary first even though it failed.
    assert_eq!(out.meta.providers_consulted, vec!["tavily", "firecrawl"]);
    assert_eq!(out.meta.strategy.as_deref(), Some("balanced"));
    // tavily retried 3× on the 500; firecrawl succeeded on its first attempt.
    assert_eq!(out.meta.attempt_count, 3 + 1);
    // sticky LAST success is the secondary leg.
    assert_eq!(out.meta.key_id, Some(fc_key.id));
}

/// Verify blend: primary succeeds, secondary + third legs fail — the response
/// is `blend-verify`, both failures land in `leg_errors`, and the first-seen
/// attempted providers include the failed legs (matching request_log).
#[tokio::test]
async fn verify_blend_keeps_results_when_other_legs_fail() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-happy-blend")
        .await
        .unwrap();
    db.insert_api_key("firecrawl", "fc-happy-blend")
        .await
        .unwrap();
    db.insert_api_key("exa", "exa-happy-blend").await.unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/search",
            body_marker: Some("\"topic\""),
            status: 200,
            body: TAVILY_OK_NO_ANSWER,
        },
        MockRoute {
            path: "/v2/search",
            body_marker: None,
            status: 500,
            body: r#"{"error":"mock firecrawl boom"}"#,
        },
        MockRoute {
            path: "/search",
            body_marker: Some("\"numResults\""),
            status: 500,
            body: r#"{"error":"mock exa boom"}"#,
        },
    ]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let body = SearchQuery {
        query: "hello".into(),
        strategy: Some("verify".into()),
        max_results: Some(5),
        ..Default::default()
    };
    let out = search_inner(&ctx, body)
        .await
        .expect("blend keeps primary success");
    assert_eq!(out.result.provider_used, "blend-verify");
    assert!(!out.result.items.is_empty(), "tavily items must survive");
    // the failed legs are reported honestly, not silently dropped
    let leg_errors = out
        .result
        .leg_errors
        .as_ref()
        .expect("failed legs reported");
    assert!(
        leg_errors.iter().any(|e| e.contains("firecrawl")),
        "secondary failure listed: {leg_errors:?}"
    );
    assert!(
        leg_errors.iter().any(|e| e.contains("exa")),
        "third-leg failure listed: {leg_errors:?}"
    );
    // first-seen attempted providers across all three legs (failed legs count)
    assert_eq!(
        out.meta.providers_consulted,
        vec!["tavily", "firecrawl", "exa"]
    );
    assert_eq!(out.meta.strategy.as_deref(), Some("verify"));
    // tavily succeeded once; firecrawl/exa retried 3× on the 500s.
    assert_eq!(out.meta.attempt_count, 1 + 3 + 3);
}

// --- (c) HYBRID web+x: both legs succeed, x answer surfaces (D3) -------------

#[tokio::test]
async fn hybrid_web_x_merges_and_surfaces_x_answer() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-happy-hybrid")
        .await
        .unwrap();
    let xai_key = db.insert_api_key("xai", "xai-happy-hybrid").await.unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/search",
            body_marker: Some("\"topic\""),
            status: 200,
            body: TAVILY_OK_NO_ANSWER,
        },
        MockRoute {
            path: "/responses",
            body_marker: None,
            status: 200,
            body: XAI_OK,
        },
    ]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let body = SearchQuery {
        query: "hello".into(),
        sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
        max_results: Some(5),
        ..Default::default()
    };
    let out = search_inner(&ctx, body).await.expect("hybrid success");
    assert_eq!(out.result.provider_used, "hybrid");
    assert!(
        out.result
            .items
            .iter()
            .any(|i| i.provider.as_deref() == Some("tavily")),
        "web-leg items merged: {:?}",
        out.result.items
    );
    assert!(
        out.result
            .items
            .iter()
            .any(|i| i.provider.as_deref() == Some("xai")),
        "x-leg items merged: {:?}",
        out.result.items
    );
    // D3/F14: the web leg returned no answer, so the xAI summary must surface.
    assert_eq!(
        out.result.answer.as_deref(),
        Some("The xAI summary answer"),
        "x-leg answer must not be discarded"
    );
    // first-seen attempted providers, web leg then x leg.
    assert_eq!(out.meta.providers_consulted, vec!["tavily", "xai"]);
    // sticky LAST success is the x leg.
    assert_eq!(out.meta.key_id, Some(xai_key.id));
    // auto + hybrid derives Balanced.
    assert_eq!(out.meta.strategy.as_deref(), Some("balanced"));
}

// --- (d) research: web / scrape / social phases all succeed ------------------

#[tokio::test]
async fn research_phases_succeed_and_wire_matches_request_log() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-happy-research")
        .await
        .unwrap();
    db.insert_api_key("firecrawl", "fc-happy-research")
        .await
        .unwrap();
    let xai_key = db
        .insert_api_key("xai", "xai-happy-research")
        .await
        .unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/search",
            body_marker: Some("\"topic\""),
            status: 200,
            body: TAVILY_OK_THREE,
        },
        // Second scrape target fails (500): the chain still records firecrawl
        // as an ATTEMPTED provider, and the page carries the error honestly.
        MockRoute {
            path: "/v2/scrape",
            body_marker: Some("r2.example"),
            status: 500,
            body: r#"{"error":"mock scrape boom"}"#,
        },
        MockRoute {
            path: "/v2/scrape",
            body_marker: None,
            status: 200,
            body: FIRECRAWL_SCRAPE_OK,
        },
        MockRoute {
            path: "/responses",
            body_marker: None,
            status: 200,
            body: XAI_OK,
        },
    ]);
    let sink = VecSink::default();
    let ctx = test_ctx_mock(db, mock, sink.clone());
    let body = ResearchRequest {
        query: "hello".into(),
        web_max_results: Some(3),
        scrape_top_n: Some(2),
        social_max_results: Some(2),
        ..Default::default()
    };
    let out = research_inner(&ctx, body).await.expect("research success");
    assert_eq!(out.result.web_results.len(), 3);
    let pages = out.result.scraped_pages.expect("scrapes ran");
    assert_eq!(pages.len(), 2, "two scrape targets");
    assert_eq!(
        pages.iter().filter(|p| p.error.is_none()).count(),
        1,
        "one scrape succeeds: {pages:?}"
    );
    assert_eq!(
        pages.iter().filter(|p| p.error.is_some()).count(),
        1,
        "one scrape fails and is reported, not dropped: {pages:?}"
    );
    assert!(
        out.result.social_results.is_some_and(|v| !v.is_empty()),
        "social leg items present"
    );
    // every phase boundary was emitted.
    let events = sink.0.lock().unwrap().clone();
    let phases: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Phase { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    for want in ["web", "scrape", "social"] {
        assert!(
            phases.iter().any(|p| p == want),
            "phase {want} emitted: {phases:?}"
        );
    }
    // D4/F15: the wire Evidence and the request_log source (meta) agree.
    let evidence = out.result.evidence.expect("evidence present");
    let wire = evidence
        .providers_consulted
        .clone()
        .expect("wire providers");
    assert_eq!(
        wire, out.meta.providers_consulted,
        "wire Evidence must match request_log meta (first-seen attempted)"
    );
    assert_eq!(wire, vec!["tavily", "firecrawl", "xai"]);
    // sticky LAST success across web → scrape → social is the social leg.
    assert_eq!(out.meta.key_id, Some(xai_key.id));
}

// --- (e) B18: structured extract end-to-end (job start + status poll) --------

#[tokio::test]
async fn structured_extract_job_start_and_poll_return_data() {
    let db = test_db().await;
    db.insert_api_key("firecrawl", "fc-happy-structured")
        .await
        .unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/v2/extract",
            body_marker: Some("\"prompt\""),
            status: 200,
            body: FIRECRAWL_EXTRACT_START,
        },
        MockRoute {
            path: "/v2/extract/job-1",
            body_marker: None,
            status: 200,
            body: FIRECRAWL_EXTRACT_COMPLETED,
        },
    ]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let out = crate::extract_structured(
        &ctx,
        "https://example.com",
        Some("extract the company name"),
        None,
        None,
    )
    .await
    .expect("structured extract success");
    let resp = out.result;
    assert_eq!(resp.provider_used, "firecrawl");
    assert_eq!(
        resp.data,
        Some(serde_json::json!({"company": {"name": "Acme Corp"}})),
        "completed job data surfaces verbatim"
    );
    assert!(
        resp.content.contains("Structured extraction"),
        "content is a human summary: {}",
        resp.content
    );
    // meta records the firecrawl attempt (request_log parity).
    assert_eq!(out.meta.providers_consulted, vec!["firecrawl"]);
}

#[tokio::test]
async fn structured_extract_rejects_non_firecrawl_provider() {
    let db = test_db().await;
    let ctx = test_ctx_mock(db, "http://127.0.0.1:9".into(), VecSink::default());
    let err = crate::extract_structured(
        &ctx,
        "https://example.com",
        Some("extract the company name"),
        None,
        Some("tavily"),
    )
    .await
    .expect_err("explicit non-firecrawl + structured must be a client error");
    assert!(
        matches!(err.result, crate::ExtractError::InvalidRequest(_)),
        "expected InvalidRequest, got {:?}",
        err.result
    );
}

#[tokio::test]
async fn structured_extract_failed_job_maps_to_provider_error() {
    let db = test_db().await;
    db.insert_api_key("firecrawl", "fc-happy-structured-fail")
        .await
        .unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/v2/extract",
            body_marker: Some("\"prompt\""),
            status: 200,
            body: FIRECRAWL_EXTRACT_START,
        },
        MockRoute {
            path: "/v2/extract/job-1",
            body_marker: None,
            status: 200,
            body: r#"{"success":true,"status":"failed","error":"blocked by robots"}"#,
        },
    ]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let err = crate::extract_structured(
        &ctx,
        "https://example.com",
        Some("extract the company name"),
        None,
        None,
    )
    .await
    .expect_err("failed job surfaces as an error");
    let message = match &err.result {
        crate::ExtractError::Provider(m) => m.clone(),
        other => panic!("expected Provider error, got {other:?}"),
    };
    assert!(
        message.contains("blocked by robots"),
        "vendor error preserved: {message}"
    );
}

// --- (f) B19: deep research ------------------------------------------------

#[tokio::test]
async fn deep_research_synthesizes_grounded_answer() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-happy-deep")
        .await
        .unwrap();
    db.insert_api_key("firecrawl", "fc-happy-deep")
        .await
        .unwrap();
    db.insert_api_key("xai", "xai-happy-deep").await.unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/search",
            body_marker: Some("\"topic\""),
            status: 200,
            body: TAVILY_OK_THREE,
        },
        MockRoute {
            path: "/v2/scrape",
            body_marker: None,
            status: 200,
            body: FIRECRAWL_SCRAPE_OK,
        },
        MockRoute {
            path: "/responses",
            body_marker: None,
            status: 200,
            body: XAI_OK,
        },
    ]);
    let sink = VecSink::default();
    let ctx = test_ctx_mock(db, mock, sink.clone());
    let body = ResearchRequest {
        query: "hello".into(),
        web_max_results: Some(3),
        scrape_top_n: Some(2),
        deep: true,
        ..Default::default()
    };
    let out = research_inner(&ctx, body)
        .await
        .expect("deep research success");
    let evidence = out.result.evidence.expect("evidence present");
    assert_eq!(
        evidence.summary.as_deref(),
        Some("The xAI summary answer"),
        "deep synthesis answer surfaces"
    );
    assert!(!out.result.web_results.is_empty(), "web results survive");
    let pages = out.result.scraped_pages.expect("scrapes ran");
    assert!(!pages.is_empty(), "scraped pages present");
    // every deep phase boundary was emitted (both iterations).
    let events = sink.0.lock().unwrap().clone();
    let phases: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            ProgressEvent::Phase { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    for want in [
        "deep-search",
        "deep-scrape",
        "deep-synthesize",
        "deep-refine",
    ] {
        assert!(
            phases.iter().any(|p| p == want),
            "phase {want} emitted: {phases:?}"
        );
    }
    // providers consulted include the synthesis attempt (first-seen order).
    assert_eq!(
        out.meta.providers_consulted,
        vec!["tavily", "firecrawl", "xai"]
    );
}

#[tokio::test]
async fn deep_research_without_xai_falls_back_without_answer() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-happy-deep2")
        .await
        .unwrap();
    db.insert_api_key("firecrawl", "fc-happy-deep2")
        .await
        .unwrap();
    db.insert_api_key("xai", "xai-happy-deep2").await.unwrap();
    let mock = spawn_mock(vec![
        MockRoute {
            path: "/search",
            body_marker: Some("\"topic\""),
            status: 200,
            body: TAVILY_OK_THREE,
        },
        MockRoute {
            path: "/v2/scrape",
            body_marker: None,
            status: 200,
            body: FIRECRAWL_SCRAPE_OK,
        },
        MockRoute {
            path: "/responses",
            body_marker: None,
            status: 500,
            body: r#"{"error":"mock xai boom"}"#,
        },
    ]);
    let ctx = test_ctx_mock(db, mock, VecSink::default());
    let body = ResearchRequest {
        query: "hello".into(),
        web_max_results: Some(3),
        scrape_top_n: Some(2),
        deep: true,
        ..Default::default()
    };
    let out = research_inner(&ctx, body)
        .await
        .expect("deep fallback succeeds as a normal research result");
    let evidence = out.result.evidence.expect("evidence present");
    assert!(
        evidence.summary.is_none(),
        "never fabricate an answer when synthesis fails: {:?}",
        evidence.summary
    );
    assert!(!out.result.web_results.is_empty(), "web results survive");
    // the synthesis failure is reported as a leg warning, not swallowed.
    let leg_errors = evidence
        .web_leg_errors
        .expect("synthesis failure reported in evidence");
    assert!(
        leg_errors.iter().any(|e| e.contains("synthesis")),
        "leg warning names the synthesis gap: {leg_errors:?}"
    );
}
