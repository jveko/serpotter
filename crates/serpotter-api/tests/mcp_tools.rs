mod common;

use common::*;

#[tokio::test]
async fn mcp_tools_list() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
async fn mcp_search_accepts_snake_case_max_results() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // no provider keys → tool returns isError true, but arg parse must succeed (not missing-field panic)
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
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
