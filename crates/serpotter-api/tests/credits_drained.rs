//! A drained vendor account is its own error CLASS on both surfaces.
//!
//! Upstream `402` used to reach the caller as a `Provider`/502 with
//! `retryable:true`, the same as a vendor 500. That is the wrong answer in
//! exactly the case that matters: the ladders reach their `402` arm on the
//! FINAL attempt, by which point every key that answered `402` has been
//! demoted (its `PaymentRequired` report zeroed the credits), so the pool a
//! retry would land in IS the drained one. `retryable:true` there points an
//! agent into a retry loop against a balance that cannot recover.
//!
//! These tests drive a real `402` end to end — scripted upstream, real key
//! pool, real ladder — and pin the wire on all three consumers: the REST
//! problem+json for SEARCH and for EXTRACT (both `503 /CreditsExhausted`,
//! `retryable:false`), the MCP tool envelope (`kind` + `retryable:false`),
//! and the admin request-log row (`errorKind` = `CreditsExhausted`, status
//! 503), so the operator funnel counts a drain distinctly from an outage.

mod common;

use std::time::Duration;

use common::*;
use serde_json::Value;

/// The vendor's own drained-account copy. Scripted on EVERY dial, so the
/// ladder runs to its final attempt and lands in the `402` arm.
const DRAINED_BODY: &str = r#"{"error":"Insufficient credits. Add credits to continue."}"#;

async fn drained_db(tag: &str) -> (serpotter_db::Db, i64) {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let key = db
        .insert_api_key("tavily", &format!("tvly-{tag}"))
        .await
        .unwrap();
    db.set_api_key_credits(key.id, Some(100)).await.unwrap();
    (db, key.id)
}

/// App whose Tavily client dials `url` (the other vendors stay on the dead
/// 127.0.0.1:9 pin, and only Tavily has a key in the pool).
fn state_draining(db: serpotter_db::Db) -> serpotter_api::AppState {
    let mut st = state_with(db);
    st.providers = serpotter_providers::ProviderRegistry::with_clients(
        serpotter_providers::TavilyClient::new(spawn_scripted(402, DRAINED_BODY)),
        serpotter_providers::FirecrawlClient::new("http://127.0.0.1:9"),
        serpotter_providers::ExaClient::new("http://127.0.0.1:9"),
        serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
    );
    st.product_config = st
        .product_config
        .with_request_timeout(Duration::from_secs(10));
    st
}

/// App whose FIRECRAWL client dials `url` — the extract-path twin of
/// `state_draining`, for `/api/extract` rather than `/api/search`.
fn state_draining_firecrawl(db: serpotter_db::Db) -> serpotter_api::AppState {
    let mut st = state_with(db);
    let url = spawn_scripted(402, DRAINED_BODY);
    st.providers = serpotter_providers::ProviderRegistry::with_clients(
        serpotter_providers::TavilyClient::new("http://127.0.0.1:9"),
        serpotter_providers::FirecrawlClient::new(url),
        serpotter_providers::ExaClient::new("http://127.0.0.1:9"),
        serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
    );
    st.product_config = st
        .product_config
        .with_request_timeout(Duration::from_secs(10));
    st
}

/// POST `/api/extract` with a per-test request id, returns `(status, body)`.
async fn extract(app: &axum::Router, request_id: &'static str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("x-request-id", request_id)
                .body(Body::from(
                    r#"{"url":"https://example.com","provider":"firecrawl"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res).await)
}

/// Admin request-log row carrying `request_id`, read back from the ring.
async fn log_row(app: axum::Router, request_id: &str) -> Value {
    for _ in 0..100 {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?limit=100")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "admin list must authorize");
        let v = body_json(res).await;
        if let Some(row) = v
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["requestId"] == request_id))
        {
            return row.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no request-log row for requestId {request_id}");
}

/// The EXTRACT surface must carry the same class as search. Without this the
/// only coverage was unit tests calling `extract_problem` with a hand-built
/// variant: a handler that mapped `ExtractError` wrongly before reaching
/// `extract_problem`, or built its own problem body, would have stayed green
/// while putting the wrong status and `retryable` on the wire.
#[tokio::test]
async fn a_drained_extract_is_503_credits_exhausted_and_not_retryable() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("firecrawl", "fc-drained-extract")
        .await
        .unwrap();
    let app = app(state_draining_firecrawl(db.clone()));

    let (status, problem) = extract(&app, "drained-extract-1").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a drained extract is 503, not a retryable 502: {problem}"
    );
    assert!(
        problem["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/CreditsExhausted"),
        "kind uri: {problem}"
    );
    assert_eq!(
        problem["retryable"], false,
        "retrying a drained extract account cannot help until it is topped up: {problem}"
    );
}

/// The doc claim this file was missing: the drained class reaches the operator
/// funnel, not just the wire. The handler derives BOTH the status and the
/// problem kind from one `extract_problem` call and feeds that same kind to
/// `fields_from_meta` — so if the ring disagreed with the wire, the funnel
/// would count drains as generic vendor errors and the alert that should say
/// "top up the account" would never fire.
#[tokio::test]
async fn a_drained_request_records_the_credits_exhausted_kind_in_the_funnel() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("firecrawl", "fc-drained-ring")
        .await
        .unwrap();
    let app = app(state_draining_firecrawl(db.clone()));

    let (status, _) = extract(&app, "drained-ring-1").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

    let row = log_row(app, "drained-ring-1").await;
    assert_eq!(
        row["errorKind"], "CreditsExhausted",
        "the funnel must classify the drain distinctly, else drains are \
         indistinguishable from vendor outages in every counter: {row}"
    );
    assert_eq!(
        row["status"], 503,
        "the logged status must be the wire status, not a separate guess: {row}"
    );
}

/// POST `/api/search` with a per-test request id, returns `(status, body)`.
async fn search(app: &axum::Router, request_id: &'static str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("x-request-id", request_id)
                .body(Body::from(
                    r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res).await)
}

/// A drained search is 503 `/CreditsExhausted` with `retryable:false` — never
/// the retryable 502 `ProviderError` an agent would loop against. The
/// request-log row must carry the same kind, so the funnel counts it as
/// distinct from a vendor outage.
#[tokio::test]
async fn drained_search_is_503_credits_exhausted_and_not_retryable() {
    let (db, key_id) = drained_db("drained-rest").await;
    let app = app(state_draining(db.clone()));

    let (status, problem) = search(&app, "drained-rest-1").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a drained pool is 503, not a retryable 502: {problem}"
    );
    assert!(
        problem["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/CreditsExhausted"),
        "kind uri: {problem}"
    );
    assert_eq!(
        problem["retryable"], false,
        "retrying a drained account cannot help until it is topped up: {problem}"
    );
    assert_eq!(
        problem["detail"], "tavily is out of credits (upstream 402)",
        "the dedicated copy must survive onto the wire: {problem}"
    );

    // The class is not only a mapping: the key really was demoted by its
    // `PaymentRequired` report, which is the fact that makes the pool drained
    // and the retry useless.
    let row = db.get_api_key_admin(key_id).await.unwrap().unwrap();
    assert_eq!(
        row.credits_remaining,
        Some(0),
        "the 402 must zero the tracked credits"
    );
    assert_eq!(row.active, 1, "402 is not a ban: the row stays, unfunded");
}

/// The `retryable:false` decision and the `503` have to be reachable from the
/// MCP envelope too — it derives both from the same kind, and a drain that
/// arrived as a retryable `ProviderError` there would be the same bug wearing
/// a different transport.
#[tokio::test]
async fn drained_search_mcp_envelope_is_credits_exhausted_and_not_retryable() {
    let (db, _key_id) = drained_db("drained-mcp").await;
    let app = app(state_draining(db));

    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    let sid = init
        .headers()
        .get("mcp-session-id")
        .or_else(|| init.headers().get("Mcp-Session-Id"))
        .expect("Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_string();
    let _ = body_json(init).await;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", &sid)
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","provider":"tavily"}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(res).await;
    assert_eq!(
        v["result"]["isError"], true,
        "a drained search must error: {v}"
    );
    let text = v["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c.get("text").or_else(|| c.get("Text")))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("error content text missing: {v}"));
    let env: Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("error envelope must be JSON: {e}: {text}"));
    assert_eq!(
        env["kind"], "CreditsExhausted",
        "the MCP kind must name the drained pool, not a generic provider fault: {env}"
    );
    assert_eq!(
        env["retryable"], false,
        "an agent that retries here loops against a dead balance: {env}"
    );
    assert!(
        env["message"]
            .as_str()
            .unwrap_or("")
            .contains("out of credits (upstream 402)"),
        "the drained copy must reach the agent: {env}"
    );
}

/// The boundary that keeps the new class honest: a `401` is a real credential
/// fault on a key that may hold credit, so it stays 502 `ProviderError` with
/// `retryable:true`. Only the drained account is terminal-without-a-refill.
#[tokio::test]
async fn an_auth_failure_on_the_same_ladder_stays_retryable_502() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // Every web leg in the fallback chain is seeded AND pointed at the same
    // scripted status, so no hop can end in a `NoHealthyKey` and mask the
    // class under test: the chain answers with its LAST provider-side failure.
    for svc in ["tavily", "exa", "firecrawl"] {
        db.insert_api_key(svc, &format!("{svc}-drained-401"))
            .await
            .unwrap();
    }
    let mut st = state_with(db);
    let url = spawn_scripted(401, r#"{"error":"Unauthorized"}"#);
    st.providers = serpotter_providers::ProviderRegistry::with_clients(
        serpotter_providers::TavilyClient::new(url.clone()),
        serpotter_providers::FirecrawlClient::new(url.clone()),
        serpotter_providers::ExaClient::new(url),
        serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
    );
    let app = app(st);

    let (status, problem) = search(&app, "drained-rest-401").await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a 401 is a provider fault, not a drained account: {problem}"
    );
    assert!(
        problem["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/ProviderError"),
        "kind uri: {problem}"
    );
    assert_eq!(
        problem["retryable"], true,
        "a 401 key may still hold credit, so the request stays retryable: {problem}"
    );
}

/// Guard against a silent regression in the OTHER direction: a plain
/// `Provider`/502 must keep `retryable:true` even though its sibling 503 kind
/// is non-retryable, so the new exclusion cannot leak into vendor outages.
#[tokio::test]
async fn a_vendor_outage_stays_a_retryable_502() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // Same seeding rule as the 401 case: every chain leg answers the same
    // status, so the answer is that status and not a keyless hop.
    for svc in ["tavily", "exa", "firecrawl"] {
        db.insert_api_key(svc, &format!("{svc}-drained-500"))
            .await
            .unwrap();
    }
    let mut st = state_with(db);
    let url = spawn_scripted(500, r#"{"error":"internal"}"#);
    st.providers = serpotter_providers::ProviderRegistry::with_clients(
        serpotter_providers::TavilyClient::new(url.clone()),
        serpotter_providers::FirecrawlClient::new(url.clone()),
        serpotter_providers::ExaClient::new(url),
        serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
    );
    st.product_config = st
        .product_config
        .with_request_timeout(Duration::from_secs(10));
    let app = app(st);

    let (status, problem) = search(&app, "drained-rest-500").await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a 500 is an outage: {problem}"
    );
    assert_eq!(
        problem["retryable"], true,
        "an outage recovers on its own: {problem}"
    );
}
