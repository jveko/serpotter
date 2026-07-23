mod common;

use common::*;

fn session_id_from(res: &axum::response::Response) -> String {
    res.headers()
        .get("mcp-session-id")
        .or_else(|| res.headers().get("Mcp-Session-Id"))
        .expect("Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn mcp_initialize_returns_session_header() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "init status");
    let sid = session_id_from(&res);
    // rmcp uses UUID session ids (opaque; no longer fixed 32-hex)
    assert!(!sid.is_empty(), "session id empty");
    let v = body_json(res).await;
    assert!(v.get("result").is_some(), "initialize result: {v}");
}

#[tokio::test]
async fn mcp_unknown_session_header_404() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", "deadbeef-dead-beef-dead-beefdeadbeef")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
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
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::OK);
    let sid = session_id_from(&init);
    // consume body so session worker can proceed
    let _ = body_json(init).await;

    let list = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", &sid)
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let v = body_json(list).await;
    assert!(
        v["result"]["tools"].as_array().unwrap().len() >= 4,
        "list: {v}"
    );
}

#[tokio::test]
async fn mcp_delete_terminates_session() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    let sid = session_id_from(&init);
    let _ = body_json(init).await;

    let del = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", &sid)
                .header("accept", MCP_ACCEPT)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // rmcp returns 202 Accepted for DELETE terminate
    assert!(
        del.status() == StatusCode::NO_CONTENT || del.status() == StatusCode::ACCEPTED,
        "delete status={}",
        del.status()
    );

    let list = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", &sid)
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
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
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    let sid = session_id_from(&init);
    let _ = body_json(init).await;

    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("mcp-session-id", &sid)
                .header("accept", "text/event-stream")
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
    drop(res);
}

#[tokio::test]
async fn mcp_requires_auth() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .body(Body::from(MCP_INIT_BODY))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_rejects_non_allowlisted_host() {
    // Default StreamableHttpServerConfig allows loopback only when MCP_ALLOWED_HOSTS unset.
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("host", "evil.example")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .body(Body::from(MCP_INIT_BODY))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        StatusCode::OK,
        "non-loopback Host must not initialize under default allowlist"
    );
}
