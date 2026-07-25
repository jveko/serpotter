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
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mysearch_health","arguments":{}}}"#,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], false, "health result: {v}");
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
