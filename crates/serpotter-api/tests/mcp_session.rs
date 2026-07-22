mod common;

use common::*;

#[tokio::test]
async fn mcp_initialize_returns_session_header() {
    let db = test_db().await;
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
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
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
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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

#[tokio::test]
async fn mcp_delete_terminates_session() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
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
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
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
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
