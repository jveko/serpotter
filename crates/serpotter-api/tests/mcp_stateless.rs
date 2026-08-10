//! MCP 2026-07-28 stateless-path coverage (dual-era: legacy sessions live in
//! `mcp_session.rs`; this suite exercises the modern per-request metadata
//! contract on the same endpoint).
//!
//! Stateless requests must carry:
//! - `MCP-Protocol-Version` header (required by `stateless_protocol_metadata_required`)
//! - `_meta.io.modelcontextprotocol/protocolVersion` + `clientCapabilities`
//! - `Mcp-Method` on every request, `Mcp-Name` on `tools/call`
//! GET/DELETE on a stateless request → 405 (sessions removed in 2026-07-28).

mod common;

use common::*;

/// Protocol version under test.
const STATELESS_VERSION: &str = "2026-07-28";

/// Build a stateless 2026-07-28 JSON-RPC request body with per-request `_meta`.
fn stateless_body(method: &str, id: i64, params: serde_json::Value) -> String {
    let mut params = params;
    let obj = params.as_object_mut().expect("params must be an object");
    obj.insert(
        "_meta".to_string(),
        serde_json::json!({
            "io.modelcontextprotocol/protocolVersion": STATELESS_VERSION,
            "io.modelcontextprotocol/clientCapabilities": {},
            "io.modelcontextprotocol/clientInfo": { "name": "serpotter-test", "version": "0.1.0" },
        }),
    );
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("serialize stateless body")
}

/// Build a stateless 2026-07-28 MCP POST with required headers + `_meta`.
fn stateless_request(method: &str, name: Option<&str>, body: String) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("MCP-Protocol-Version", STATELESS_VERSION)
        .header("Mcp-Method", method);
    if let Some(name) = name {
        builder = builder.header("Mcp-Name", name);
    }
    builder.body(Body::from(body)).unwrap()
}

#[tokio::test]
async fn mcp_discover_lists_2026_07_28_and_tools_capability() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(stateless_request(
            "server/discover",
            None,
            stateless_body("server/discover", 1, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "discover status");
    let v = body_json(res).await;
    let result = v.get("result").expect("discover result");
    let versions = result["supportedVersions"]
        .as_array()
        .expect("supportedVersions array");
    assert!(
        versions
            .iter()
            .any(|ver| ver.as_str() == Some(STATELESS_VERSION)),
        "supportedVersions must include 2026-07-28: {result}"
    );
    assert!(
        result["capabilities"]["tools"].is_object(),
        "tools capability advertised: {result}"
    );
}

#[tokio::test]
async fn mcp_stateless_tools_list_without_session() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(stateless_request(
            "tools/list",
            None,
            stateless_body("tools/list", 2, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "stateless tools/list status");
    let v = body_json(res).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    assert!(tools.len() >= 4, "tools: {v}");
    assert!(
        v["result"]["resultType"].as_str() == Some("complete"),
        "resultType required by 2026-07-28: {v}"
    );
}

#[tokio::test]
async fn mcp_stateless_tools_call_with_headers() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("health"),
            stateless_body(
                "tools/call",
                3,
                serde_json::json!({"name": "health", "arguments": {}}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "stateless tools/call status");
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], false, "health result: {v}");
    assert!(
        v["result"]["content"].as_array().is_some(),
        "content present: {v}"
    );
}

#[tokio::test]
async fn mcp_stateless_missing_protocol_version_header_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    // No MCP-Protocol-Version header, but _meta carries 2026-07-28 →
    // validate_request_protocol_version_meta demands the matching header.
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("Mcp-Method", "tools/list")
        .body(Body::from(stateless_body(
            "tools/list",
            4,
            serde_json::json!({}),
        )))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "missing header → 400"
    );
}

#[tokio::test]
async fn mcp_stateless_header_body_version_mismatch_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    // Header says 2026-07-28 but body _meta says 2025-11-25 → HeaderMismatch.
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("MCP-Protocol-Version", STATELESS_VERSION)
        .header("Mcp-Method", "tools/list")
        .body(Body::from(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2025-11-25","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "version mismatch → 400"
    );
}

#[tokio::test]
async fn mcp_stateless_missing_mcp_method_header_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    // 2026-07-28 requires Mcp-Method on every request (SEP-2243); missing → 400.
    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("MCP-Protocol-Version", STATELESS_VERSION)
        .body(Body::from(stateless_body(
            "tools/list",
            6,
            serde_json::json!({}),
        )))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "missing Mcp-Method → 400"
    );
}

#[tokio::test]
async fn mcp_stateless_get_405() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    // GET on the stateless path: sessions/stream removed → 405 (no event store).
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("MCP-Protocol-Version", STATELESS_VERSION)
                .header("accept", "text/event-stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "GET → 405");
}

#[tokio::test]
async fn mcp_stateless_delete_405() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("MCP-Protocol-Version", STATELESS_VERSION)
                .header("accept", MCP_ACCEPT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED, "DELETE → 405");
}

#[tokio::test]
async fn mcp_stateless_requires_auth() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/list",
            None,
            stateless_body("tools/list", 7, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// A stateless tools/call whose _meta carries a progressToken must stream
/// notifications/progress (SSE) and end with the terminal result.
#[tokio::test]
async fn mcp_stateless_search_with_progress_token_streams_sse() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-progress").await.unwrap();
    let app = app(state_with(db));

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 30,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "progressToken": "tok-abc-123"
            },
            "name": "search",
            "arguments": { "query": "hello", "max_results": 1 }
        }
    });
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            serde_json::to_string(&body).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "with progressToken the response must be SSE, got content-type={ct}"
    );
    let text = String::from_utf8(body_bytes(res).await.to_vec()).unwrap();
    assert!(
        text.contains("notifications/progress"),
        "SSE must carry notifications/progress frames: {text}"
    );
    assert!(
        text.contains("progressToken") || text.contains("\"token\":\"tok-abc-123\""),
        "progress frames must echo the client token: {text}"
    );
}

/// research must stream notifications/progress when _meta carries a
/// progressToken: its sink is built from the explicit peer/meta handler
/// params (rmcp's RequestMetaObject extractor swaps meta out of the
/// context), so this covers the path a search-style context.meta read
/// would silently break.
#[tokio::test]
async fn mcp_stateless_research_with_progress_token_streams_sse() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-progress").await.unwrap();
    let app = app(state_with(db));

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 32,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
                "progressToken": "tok-research-1"
            },
            "name": "research",
            "arguments": { "query": "hello", "web_max_results": 1, "scrape_top_n": 0 }
        }
    });
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("research"),
            serde_json::to_string(&body).unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "with progressToken the response must be SSE, got content-type={ct}"
    );
    let text = String::from_utf8(body_bytes(res).await.to_vec()).unwrap();
    assert!(
        text.contains("notifications/progress"),
        "SSE must carry notifications/progress frames: {text}"
    );
    assert!(
        text.contains("progressToken") || text.contains("\"token\":\"tok-research-1\""),
        "progress frames must echo the client token: {text}"
    );
}

/// Without a progressToken the fast path stays plain JSON.
#[tokio::test]
async fn mcp_stateless_search_without_token_stays_json() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            stateless_body("tools/call", 31, serde_json::json!({"name": "search", "arguments": {"query": "hello", "max_results": 1}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        ct.starts_with("application/json"),
        "without progressToken response must stay plain JSON, got content-type={ct}"
    );
    let v = body_json(res).await;
    assert!(v.get("result").is_some(), "terminal result present: {v}");
}
