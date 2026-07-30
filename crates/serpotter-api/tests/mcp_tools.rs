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
    let ann = search
        .get("annotations")
        .expect("search tool annotations");
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
    assert_eq!(body["status"], "ready", "migrated fixture must be ready: {body}");
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
