mod common;

use common::*;

async fn init_session(app: &axum::Router) -> String {
    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::OK, "initialize failed");
    let sid = init
        .headers()
        .get("mcp-session-id")
        .or_else(|| init.headers().get("Mcp-Session-Id"))
        .expect("Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_string();
    let _ = body_json(init).await;
    sid
}

fn mcp_session_request(sid: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("mcp-session-id", sid)
        .body(body.into())
        .unwrap()
}

#[tokio::test]
async fn mcp_tools_list() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let tools = v["result"]["tools"].as_array().expect("tools array");
    assert!(tools.len() >= 4, "tools: {v}");
    let search = tools
        .iter()
        .find(|t| t["name"] == "search")
        .expect("search tool");
    let schema = &search["inputSchema"];
    let props = schema
        .get("properties")
        .or_else(|| schema.get("schema").and_then(|s| s.get("properties")))
        .unwrap_or(schema);
    let props_str = props.to_string();
    assert!(
        props_str.contains("strategy"),
        "search inputSchema should expose strategy: {schema}"
    );
    // ToolAnnotations (openWorld/readOnly) should surface on tools/list
    let ann = search.get("annotations").expect("search tool annotations");
    assert_eq!(ann["readOnlyHint"], true, "search annotations: {ann}");
    assert_eq!(ann["openWorldHint"], true, "search annotations: {ann}");
}

#[tokio::test]
async fn mcp_search_accepts_strategy() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","strategy":"fast","max_results":3}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(
        v.get("error").is_none(),
        "strategy must not cause protocol error: {v}"
    );
    assert!(
        v.get("result").is_some(),
        "expected tools/call result envelope: {v}"
    );
    let result = &v["result"];
    assert!(
        result.get("content").is_some() || result.get("isError").is_some(),
        "unexpected result: {result}"
    );
}

#[tokio::test]
async fn mcp_health_tool() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"health","arguments":{}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], false, "health result: {v}");
    let text = v["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c.get("text").or_else(|| c.get("Text")))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("health content text missing: {v}"));
    let body: serde_json::Value =
        serde_json::from_str(text).unwrap_or_else(|e| panic!("health body JSON: {e}: {text}"));
    assert_eq!(
        body["status"], "ready",
        "migrated fixture must be ready: {body}"
    );
    assert!(
        body["schemaVersion"].as_i64().is_some(),
        "schemaVersion present: {body}"
    );
    assert!(
        body["expected"].as_i64().is_some(),
        "expected present: {body}"
    );
    assert!(
        body["schemaVersion"].as_i64().unwrap() >= body["expected"].as_i64().unwrap(),
        "schemaVersion >= expected: {body}"
    );
}

#[tokio::test]
async fn mcp_search_accepts_snake_case_max_results() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","max_results":3}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(
        v.get("result").is_some(),
        "expected tools/call result envelope: {v}"
    );
    let result = &v["result"];
    assert!(
        result.get("content").is_some() || result.get("isError").is_some(),
        "unexpected result: {result}"
    );
}

#[tokio::test]
async fn mcp_unknown_tool_is_protocol_error() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(
        v.get("error").is_some(),
        "unknown tool must be JSON-RPC protocol error (not result.isError): {v}"
    );
    assert!(
        v.get("result").is_none(),
        "unknown tool must not return tools/call result envelope: {v}"
    );
}

#[tokio::test]
async fn mcp_research_accepts_include_content_alias() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;

    let list = app
        .clone()
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/list"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let list_v = body_json(list).await;
    let tools = list_v["result"]["tools"].as_array().expect("tools array");
    let research = tools
        .iter()
        .find(|t| t["name"] == "research")
        .expect("research tool");
    let schema = &research["inputSchema"];
    let props = schema
        .get("properties")
        .or_else(|| schema.get("schema").and_then(|s| s.get("properties")))
        .unwrap_or(schema);
    let props_str = props.to_string();
    assert!(
        props_str.contains("include_content") || props_str.contains("includeContent"),
        "research inputSchema should expose include_content: {schema}"
    );

    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"research","arguments":{"query":"hello","includeContent":true,"web_max_results":2,"scrape_top_n":0}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(
        v.get("error").is_none(),
        "includeContent alias must not cause protocol error: {v}"
    );
    assert!(
        v.get("result").is_some(),
        "expected tools/call result envelope: {v}"
    );
    let result = &v["result"];
    assert!(
        result.get("content").is_some() || result.get("isError").is_some(),
        "unexpected result: {result}"
    );
}

#[tokio::test]
async fn mcp_tools_call_logs_token_name() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "mcp-local").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;

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
                .header("x-request-id", "mcp-req-token-1")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","max_results":1}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let _ = body_json(res).await;

    // spawn_log is fire-and-forget — poll until the row lands.
    let mut found = None;
    for _ in 0..50 {
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
        let rows = v.as_array().expect("logs array");
        if let Some(row) = rows.iter().find(|r| r["path"] == "/mcp/search") {
            found = Some(row.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let row = found.expect("expected /mcp/search request_log row after tools/call");
    assert_eq!(
        row["tokenName"], "mcp-local",
        "MCP must populate token_name when tok- valid: {row}"
    );
    assert_eq!(
        row["requestId"], "mcp-req-token-1",
        "MCP should forward x-request-id: {row}"
    );
}

/// Contract #1: every MCP tool failure carries one JSON envelope text block
/// {"kind","message","requestId"} with a machine-readable stable kind.
/// No api keys → search fails NoHealthyKey; the echoed x-request-id must land
/// in requestId (never lost to the client).
#[tokio::test]
async fn mcp_search_error_envelope_kind_and_request_id() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", &sid)
                .header("x-request-id", "mcp-err-env-1")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello"}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(
        v["result"]["isError"], true,
        "search without keys must error: {v}"
    );
    let text = v["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c.get("text").or_else(|| c.get("Text")))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("error content text missing: {v}"));
    let env: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("error envelope must be JSON: {e}: {text}"));
    assert_eq!(env["kind"], "NoHealthyKey", "stable kind: {env}");
    assert!(
        env["message"]
            .as_str()
            .unwrap_or("")
            .contains("search failed:"),
        "display message preserved: {env}"
    );
    assert_eq!(
        env["requestId"], "mcp-err-env-1",
        "requestId echoed from x-request-id: {env}"
    );
}

/// MCP routing knobs (mode/intent/strategy/provider) advertise closed sets in
/// schemars; non-empty values outside those sets must be rejected with the
/// ValidationError envelope instead of silently coercing (strategy -> fast,
/// mode -> no-op) inside routing.
#[tokio::test]
async fn mcp_search_rejects_unknown_routing_values() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;

    for (field, value) in [
        ("strategy", "banana"),
        ("mode", "silly"),
        ("intent", "mystery"),
        ("provider", "google"),
    ] {
        let res = app
            .clone()
            .oneshot(mcp_session_request(
                &sid,
                format!(
                    r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"search","arguments":{{"query":"hello","{field}":"{value}"}}}}}}"#
                ),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        assert_eq!(
            v["result"]["isError"], true,
            "{field}={value} must be rejected: {v}"
        );
        let text = v["result"]["content"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|c| c.get("text").or_else(|| c.get("Text")))
            .and_then(|t| t.as_str())
            .unwrap_or_else(|| panic!("error content text missing: {v}"));
        let env: serde_json::Value = serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("error envelope must be JSON: {e}: {text}"));
        assert_eq!(env["kind"], "ValidationError", "{field}: {env}");
        assert!(
            env["message"]
                .as_str()
                .unwrap_or("")
                .contains("not a supported value"),
            "message must name the valid set: {env}"
        );
    }
}

/// notifications/cancelled must abort an in-flight tool call early. The search
/// waits on an at-cap key pool for a 30s acquire timeout; the cancel reaches
/// the handler via rmcp's request CancellationToken and lands a 499/Cancelled
/// request_log row long before the acquire deadline (request_id 200).
#[tokio::test]
async fn mcp_search_cancelled_mid_flight_aborts() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-pool-pin").await.unwrap();
    // Pin the single tavily key at cap (max_inflight=1) so search's acquire
    // waits the full acquire_timeout: a long, observable in-flight window.
    let st = state_with_key_pool(db.clone(), 1, std::time::Duration::from_secs(30), 60);
    let _lease = st.keys.acquire("tavily").await.expect("lease tavily key");
    let app = app(st);
    let sid = init_session(&app).await;

    let in_flight = app.clone().oneshot(mcp_session_request(
        &sid,
        r#"{"jsonrpc":"2.0","id":200,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello"}}}"#,
    ));
    let handle = tokio::spawn(in_flight);

    // Send notifications/cancelled in a retry loop: it is a no-op until the
    // request id registers in the session pool; the first one that lands
    // aborts the handler, which logs the 499/Cancelled row we wait for.
    let mut found = false;
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let res = app
            .clone()
            .oneshot(mcp_session_request(
                &sid,
                r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":200}}"#,
            ))
            .await
            .unwrap();
        assert!(
            res.status().is_success(),
            "cancelled notification accepted: {}",
            res.status()
        );
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?path=/mcp/search&limit=10")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        if v.as_array()
            .is_some_and(|rows| rows.iter().any(|r| r["errorKind"] == "Cancelled"))
        {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "in-flight request must be cancelled early (499/Cancelled log row), not run to the 30s deadline"
    );

    // rmcp drops the response for a cancelled request, so the tools/call POST
    // never resolves; drop the handle and let session teardown clean it up.
    drop(handle);
}

/// MCP validation failures also use the envelope with kind ValidationError.
/// The request-id layer mints an id for every HTTP request even without an
/// inbound x-request-id, so the envelope always carries a minted one.
#[tokio::test]
async fn mcp_extract_validation_error_envelope() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":43,"method":"tools/call","params":{"name":"extract_url","arguments":{"url":""}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], true, "empty url must error: {v}");
    let text = v["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c.get("text").or_else(|| c.get("Text")))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("error content text missing: {v}"));
    let env: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("error envelope must be JSON: {e}: {text}"));
    assert_eq!(env["kind"], "ValidationError", "validation kind: {env}");
    assert_eq!(env["message"], "missing url", "validation message: {env}");
    let rid = env
        .get("requestId")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("requestId must be minted id: {env}"));
    assert!(!rid.is_empty(), "requestId must be non-empty: {env}");
}

/// F19 on the legacy session path too: type-invalid tool args (max_results as
/// a string) reach the handler and come back as the ValidationError envelope,
/// never a bare rmcp deserialization error.
#[tokio::test]
async fn mcp_type_invalid_args_envelope_legacy_session() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(&app).await;
    let res = app
        .oneshot(mcp_session_request(
            &sid,
            r#"{"jsonrpc":"2.0","id":44,"method":"tools/call","params":{"name":"search","arguments":{"query":"hello","max_results":"abc"}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(
        v["result"]["isError"], true,
        "type-invalid args must error: {v}"
    );
    let text = v["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c.get("text").or_else(|| c.get("Text")))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("error content text missing: {v}"));
    let env: serde_json::Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("error envelope must be JSON: {e}: {text}"));
    assert_eq!(env["kind"], "ValidationError", "stable kind: {env}");
    assert!(
        env["message"]
            .as_str()
            .unwrap_or("")
            .contains("invalid args"),
        "message names the parse failure: {env}"
    );
    assert!(
        env["requestId"].as_str().is_some_and(|r| !r.is_empty()),
        "requestId present: {env}"
    );
}
