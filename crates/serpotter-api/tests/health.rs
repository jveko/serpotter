use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use serpotter_api::{app, AppState};
use serpotter_db::connect_and_migrate;
use serpotter_keypool::KeyPool;
use serpotter_providers::{
    ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
};
use tower::ServiceExt;

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn state_with(db: serpotter_db::Db) -> AppState {
    AppState {
        keys: Arc::new(KeyPool::new(db.clone())),
        providers: ProviderRegistry {
            tavily: TavilyClient::new("http://127.0.0.1:9"),
            firecrawl: FirecrawlClient::new("http://127.0.0.1:9"),
            exa: ExaClient::new("http://127.0.0.1:9"),
            xai: XaiClient::new("http://127.0.0.1:9"),
        },
        db,
        admin_secret: Some("test-admin-secret".into()),
        mcp_sessions: serpotter_api::McpSessionStore::new(),
    }
}

#[tokio::test]
async fn live_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn ready_ok_schema_v7() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["schemaVersion"], 7);
    assert_eq!(v["expected"], 7);
}

#[tokio::test]
async fn search_missing_token_401() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
}

#[tokio::test]
async fn search_no_key_503() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["title"], "No Healthy Key");
}

#[tokio::test]
async fn extract_missing_token_401() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn research_missing_query_400() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"  "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_stats_with_secret() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", "Bearer test-admin-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["tokens"], 1);
    assert_eq!(v["schemaVersion"], 7);
    assert_eq!(v["requestLogs"], 0);
    assert!(v["byService"].is_array());
}

#[tokio::test]
async fn admin_rejects_without_secret() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/api/stats").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sync_credits_requires_admin() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/sync-credits")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sync_credits_empty_keys_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    // Providers point at 127.0.0.1:9; empty key list avoids network.
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/sync-credits")
                .header("Authorization", "Bearer test-admin-secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["service"], "tavily");
    assert_eq!(v["synced"], 0);
    assert_eq!(v["errors"], 0);
    assert_eq!(v["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn sync_credits_fetch_fail_keeps_key_active() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-soft-fail").await.unwrap();
    // Providers point at 127.0.0.1:9 → connection refused → soft error, not deactivate.
    let app = app(state_with(db.clone()));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/sync-credits")
                .header("Authorization", "Bearer test-admin-secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["synced"], 0);
    assert!(v["errors"].as_i64().unwrap() >= 1);
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1, "fetch fail must not set active=0");
}

#[tokio::test]
async fn mcp_tools_list() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v["result"]["tools"].as_array().unwrap().len() >= 4);
}

#[tokio::test]
async fn mcp_health_tool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mysearch_health","arguments":{}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], false);
}

#[tokio::test]
async fn mcp_initialize_returns_session_header() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let sid = res
        .headers()
        .get("mcp-session-id")
        .expect("Mcp-Session-Id")
        .to_str()
        .unwrap();
    assert_eq!(sid.len(), 32);
}

#[tokio::test]
async fn mcp_unknown_session_header_404() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("mcp-session-id", "deadbeefdeadbeefdeadbeefdeadbeef")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mcp_tools_list_with_session_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));

    let init = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::OK);
    let sid = init
        .headers()
        .get("mcp-session-id")
        .expect("session id")
        .to_str()
        .unwrap()
        .to_string();

    let list = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("mcp-session-id", &sid)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let v = body_json(list).await;
    assert!(v["result"]["tools"].as_array().unwrap().len() >= 4);
}

#[test]
fn research_request_accepts_web_max_results_aliases() {
    // mysearch REST: webMaxResults / scrapeTopN
    let body = r#"{"query":"q","webMaxResults":3,"scrapeTopN":1}"#;
    let req: serpotter_api::ResearchRequest = serde_json::from_str(body).unwrap();
    assert_eq!(req.web_max_results, Some(3));
    assert_eq!(req.scrape_top_n, Some(1));

    let body2 = r#"{"query":"q","maxResults":4,"extractTopN":2}"#;
    let req2: serpotter_api::ResearchRequest = serde_json::from_str(body2).unwrap();
    assert_eq!(req2.web_max_results, Some(4));
    assert_eq!(req2.scrape_top_n, Some(2));
}

#[tokio::test]
async fn mcp_search_accepts_snake_case_max_results() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    // no provider keys → tool returns isError true, but arg parse must succeed (not missing-field panic)
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","max_results":3}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    // must not be JSON-RPC method/arg schema failure at HTTP layer
    assert!(v.get("result").is_some(), "expected tools/call result envelope: {v}");
    // tool may error on no keys; either isError true with message, or success
    let result = &v["result"];
    assert!(
        result.get("content").is_some() || result.get("isError").is_some(),
        "unexpected result: {result}"
    );
}

#[tokio::test]
async fn research_success_body_has_web_results_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    // no api keys → 503 NoHealthyKey before body; seed a fake key so search attempts provider and fails open?
    // With no keys: 503 — still assert we don't return old {search, extracts} shape on success path.
    // Seed key so routing runs; provider points at 127.0.0.1:9 → 502 ProviderError, not research success.
    // For success-shaped body without network: unit-test ResearchResponse serde instead.
    let sample = serpotter_api::ResearchResponse {
        query: "q".into(),
        web_results: vec![],
        social_results: None,
        scraped_pages: Some(vec![]),
        citations: None,
        evidence: None,
    };
    let v = serde_json::to_value(&sample).unwrap();
    assert!(v.get("webResults").is_some(), "expected camelCase webResults: {v}");
    assert!(v.get("scrapedPages").is_some(), "expected scrapedPages: {v}");
    assert!(v.get("search").is_none());
    assert!(v.get("extracts").is_none());

    // HTTP path with token + empty query already covered; with valid query and no keys → 503
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"query":"hello","webMaxResults":3,"scrapeTopN":1}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    // no keys → 503; body is problem+json, not research success
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["title"], "No Healthy Key");
}

#[test]
fn research_response_serializes_social_results_when_some() {
    let sample = serpotter_api::ResearchResponse {
        query: "q".into(),
        web_results: vec![],
        social_results: Some(vec![]),
        scraped_pages: None,
        citations: None,
        evidence: None,
    };
    let v = serde_json::to_value(&sample).unwrap();
    assert!(v.get("socialResults").is_some());
    assert_eq!(v["socialResults"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn admin_settings_durable_roundtrip() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("Authorization", "Bearer test-admin-secret")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"socialEnabled":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let put_v = body_json(put).await;
    assert_eq!(put_v["socialEnabled"], false);
    // note must not claim in-memory stub
    if let Some(n) = put_v.get("note").and_then(|x| x.as_str()) {
        assert!(!n.contains("in-memory"));
    }

    let get = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/settings")
                .header("Authorization", "Bearer test-admin-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let get_v = body_json(get).await;
    assert_eq!(get_v["socialEnabled"], false);
}

#[tokio::test]
async fn mcp_delete_terminates_session() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let init = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let sid = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let del = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("mcp-session-id", &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);

    let list = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("mcp-session-id", &sid)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn mcp_get_sse_content_type() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));

    let init = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let sid = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("mcp-session-id", &sid)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/event-stream"),
        "content-type={ct}"
    );
    // Drop response without reading body forever (abort / no hang).
    drop(res);
}
