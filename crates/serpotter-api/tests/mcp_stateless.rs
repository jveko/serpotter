//! MCP 2026-07-28 stateless-path coverage (dual-era: legacy sessions live in
//! `mcp_session.rs`; this suite exercises the modern per-request metadata
//! contract on the same endpoint).
//!
//! Stateless requests must carry:
//! - `MCP-Protocol-Version` header (required by `stateless_protocol_metadata_required`)
//! - `_meta.io.modelcontextprotocol/protocolVersion` + `clientCapabilities`
//! - `Mcp-Method` on every request, `Mcp-Name` on `tools/call`
//!   GET/DELETE on a stateless request → 405 (sessions removed in 2026-07-28).

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
        text.contains("\"progressToken\":\"tok-abc-123\""),
        "progress frames must echo the client token verbatim: {text}"
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
        text.contains("\"progressToken\":\"tok-research-1\""),
        "progress frames must echo the client token verbatim: {text}"
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

/// Search result carries structuredContent identical to the text block.
/// Providers are pinned at 127.0.0.1:9, so this exercises the error envelope
/// path; success-path parity is covered at unit level
/// (progress.rs `structured_ok_carries_both_content_and_structured`).
#[tokio::test]
async fn mcp_stateless_search_structured_content() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-structured")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            stateless_body("tools/call", 40, serde_json::json!({"name": "search", "arguments": {"query": "hello", "max_results": 1}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let result = &v["result"];
    let structured = result["structuredContent"]
        .as_object()
        .cloned()
        .unwrap_or_else(|| panic!("structuredContent must be an object: {result}"));
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("text block present: {result}"));
    let text_v: serde_json::Value = serde_json::from_str(text).expect("text is JSON");
    assert_eq!(
        serde_json::Value::Object(structured),
        text_v,
        "structured == text"
    );
}

/// Error envelope is machine-readable in structuredContent.
#[tokio::test]
async fn mcp_stateless_error_is_structured() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("extract_url"),
            stateless_body(
                "tools/call",
                41,
                serde_json::json!({"name": "extract_url", "arguments": {"url": ""}}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], true);
    assert_eq!(v["result"]["structuredContent"]["kind"], "ValidationError");
}

/// tools/list advertises outputSchema on the three result tools.
#[tokio::test]
async fn mcp_tools_list_advertises_output_schema() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/list",
            None,
            stateless_body("tools/list", 42, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    for name in ["search", "extract_url", "research"] {
        let tool = tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} present"));
        let schema = tool["outputSchema"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} outputSchema present"));
        assert_eq!(schema["type"], "object", "{name} outputSchema root type");
    }
    // camelCase spot-check: response schemas derive from `serde(rename_all =
    // "camelCase")` structs, so multi-word keys must appear camelCase, not
    // snake_case, on the wire (1-2 asserts per tool).
    let schema = |name: &str| {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} present"))["outputSchema"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} outputSchema present"))
    };
    assert!(
        schema("search")["properties"]["providerUsed"].is_object(),
        "search outputSchema properties camelCase"
    );
    assert!(
        schema("research")["properties"]["webResults"].is_object(),
        "research outputSchema properties camelCase"
    );
    assert!(
        schema("extract_url")["properties"]["providerUsed"].is_object(),
        "extract_url outputSchema properties camelCase"
    );
    // health: no outputSchema (YAGNI)
    let health = tools
        .iter()
        .find(|t| t["name"] == "health")
        .expect("health present");
    assert!(
        health.get("outputSchema").is_none(),
        "health has no outputSchema"
    );
}

// --- F19: type-invalid tool args return the error envelope -------------------
// rmcp's typed `Parameters<T>` extraction fails before the handler with a
// bare error; the tools now receive raw args and map deserialization
// failures to the standard {kind,message,requestId} envelope.

#[tokio::test]
async fn mcp_stateless_type_invalid_args_get_envelope() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            stateless_body("tools/call", 50, serde_json::json!({"name": "search", "arguments": {"query": "hello", "max_results": "abc"}})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(
        v["result"]["isError"], true,
        "type-invalid args must error: {v}"
    );
    assert_eq!(v["result"]["structuredContent"]["kind"], "ValidationError");
    assert!(
        v["result"]["structuredContent"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("invalid args"),
        "message explains the parse failure: {v}"
    );
    let rid = v["result"]["structuredContent"]["requestId"]
        .as_str()
        .unwrap_or_else(|| panic!("requestId must be present: {v}"));
    assert!(!rid.is_empty(), "requestId non-empty: {v}");
}

/// extract_url with a number where the url string is expected → same envelope.
#[tokio::test]
async fn mcp_stateless_extract_type_invalid_args_get_envelope() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("extract_url"),
            stateless_body(
                "tools/call",
                51,
                serde_json::json!({"name": "extract_url", "arguments": {"url": 123}}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    assert_eq!(v["result"]["structuredContent"]["kind"], "ValidationError");
    assert!(
        v["result"]["structuredContent"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("invalid args"),
        "{v}"
    );
}

// --- F10: overall request deadline (MCP) -------------------------------------

/// A tool call blocked on an at-cap key pool exceeds the 1s request deadline:
/// the select!'s sleep branch fires, answering the Timeout envelope and
/// logging a 504/Timeout request_log row.
#[tokio::test]
async fn mcp_stateless_search_timeout_envelope_and_504_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-timeout").await.unwrap();
    // Pin the only xai key at cap so the tool call's acquire waits the full
    // 30s acquire timeout; the 1s request deadline fires first.
    std::env::set_var("REQUEST_TIMEOUT_SECS", "1");
    let st = state_with_key_pool(
        db.clone(),
        1,
        std::time::Duration::from_secs(30),
        serpotter_db::KEY_HOLD_TTL_SECS,
    );
    let _lease = st.keys.acquire("xai").await.expect("lease xai key");
    let app = app(st);

    let res = app
        .clone()
        .oneshot(stateless_request(
            "tools/call",
            Some("search"),
            stateless_body("tools/call", 60, serde_json::json!({"name": "search", "arguments": {"query": "hello", "provider": "xai"}})),
        ))
        .await
        .unwrap();
    std::env::remove_var("REQUEST_TIMEOUT_SECS");
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "MCP answers 200 with the envelope"
    );
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], true, "deadline must fire: {v}");
    assert_eq!(v["result"]["structuredContent"]["kind"], "Timeout");
    assert!(
        v["result"]["structuredContent"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("deadline"),
        "{v}"
    );

    // spawn_log is fire-and-forget — poll until the 504/Timeout row lands.
    let mut found = false;
    for _ in 0..100 {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?path=/mcp/&limit=20")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        if v.as_array().is_some_and(|rows| {
            rows.iter()
                .any(|r| r["errorKind"] == "Timeout" && r["status"] == 504)
        }) {
            found = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(found, "expected 504/Timeout request_log row after deadline");
}

// --- F22: Origin validation + Mcp-Name enforcement (SEP-2243) ----------------

/// Like [`stateless_request`] plus an explicit `Origin` header (origin
/// validation tests; missing-Origin requests pass even when allowlisted).
fn stateless_request_with_origin(
    method: &str,
    name: Option<&str>,
    origin: &str,
    body: String,
) -> Request<Body> {
    let mut req = stateless_request(method, name, body);
    req.headers_mut().insert(
        axum::http::header::ORIGIN,
        axum::http::HeaderValue::from_str(origin).expect("valid origin"),
    );
    req
}

/// MCP_ALLOWED_ORIGINS is wired at service() build time (mcp/mod.rs:115-130);
/// a disallowed Origin is rejected 403 by rmcp's validate_origin_header, an
/// allowed one proceeds, and a missing Origin still passes (RFC 6454:
/// validation only fires when the header is present).
#[tokio::test]
async fn mcp_stateless_origin_validation_enforces_allowlist() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    std::env::set_var("MCP_ALLOWED_ORIGINS", "https://allowed.example");
    let app = app(state_with(db));
    std::env::remove_var("MCP_ALLOWED_ORIGINS");

    // Disallowed origin → 403 Forbidden (docs/ops/api.md:68 contract).
    let res = app
        .clone()
        .oneshot(stateless_request_with_origin(
            "tools/list",
            None,
            "https://evil.example",
            stateless_body("tools/list", 70, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::FORBIDDEN,
        "disallowed Origin must be rejected 403"
    );

    // Allowed origin → the request proceeds (tools/list answers normally).
    let res = app
        .clone()
        .oneshot(stateless_request_with_origin(
            "tools/list",
            None,
            "https://allowed.example",
            stateless_body("tools/list", 71, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "allowed Origin must proceed");
    let v = body_json(res).await;
    assert!(
        v.get("result").is_some(),
        "allowed-origin request must reach the handler: {v}"
    );

    // Missing Origin passes even with the allowlist set (validation is
    // header-gated, not request-gated).
    let res = app
        .oneshot(stateless_request(
            "tools/list",
            None,
            stateless_body("tools/list", 72, serde_json::json!({})),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "missing Origin still passes");
}

/// SEP-2243 standard headers: a tools/call whose Mcp-Name header does not
/// match the body's tool name is rejected with 400 (validate_standard_headers
/// → header_mismatch_jsonrpc_response). Previously only the missing
/// Mcp-Method case was covered.
#[tokio::test]
async fn mcp_stateless_mcp_name_mismatch_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(stateless_request(
            "tools/call",
            Some("wrongtool"),
            stateless_body(
                "tools/call",
                73,
                serde_json::json!({"name": "search", "arguments": {"query": "hello"}}),
            ),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "Mcp-Name mismatch → 400"
    );
}

// --- F23: stateless-path cancellation (499/Cancelled) ------------------------

/// Client disconnect on the stateless 2026-07-28 path cancels in-flight work:
/// rmcp's per-request drop_guard (tower.rs serve_negotiated_request_directly)
/// fires when the response future is dropped, the handler's ct.cancelled()
/// branch lands a 499/Cancelled request_log row. Mirrors the legacy
/// notifications/cancelled test but without any session.
#[tokio::test]
async fn mcp_stateless_search_cancelled_on_disconnect_499() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-stateless-pin")
        .await
        .unwrap();
    // Pin the single tavily key at cap (max_inflight=1) so the search's key
    // acquire waits the full 30s acquire timeout: a long, observable
    // in-flight window. Default request deadline is 120s, so only the abort
    // can resolve the select early.
    let st = state_with_key_pool(db.clone(), 1, std::time::Duration::from_secs(30), 60);
    let _lease = st.keys.acquire("tavily").await.expect("lease tavily key");
    let app = app(st);

    let req = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("MCP-Protocol-Version", STATELESS_VERSION)
        .header("Mcp-Method", "tools/call")
        .header("Mcp-Name", "search")
        .header("x-request-id", "mcp-stateless-cancel-1")
        .body(Body::from(stateless_body(
            "tools/call",
            80,
            serde_json::json!({"name": "search", "arguments": {"query": "hello"}}),
        )))
        .unwrap();
    let handle = tokio::spawn(app.clone().oneshot(req));

    // Let the request reach the handler's in-flight select (it is blocked in
    // key acquire), then abort the response future — the stateless analogue of
    // the client closing the stream. rmcp's drop_guard cancels the per-request
    // token; the handler logs 499/Cancelled long before the 30s acquire
    // deadline.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    handle.abort();

    // spawn_log is fire-and-forget — poll until the 499/Cancelled row lands.
    let mut found = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?path=/mcp/search&limit=20")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        if v.as_array().is_some_and(|rows| {
            rows.iter().any(|r| {
                r["errorKind"] == "Cancelled"
                    && r["status"] == 499
                    && r["requestId"] == "mcp-stateless-cancel-1"
            })
        }) {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "expected 499/Cancelled request_log row after stateless disconnect"
    );
}
