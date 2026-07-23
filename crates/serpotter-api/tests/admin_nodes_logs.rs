mod common;

use common::*;

#[tokio::test]
async fn toggle_node_flips_enabled() {
    let db = test_db().await;
    let node = db
        .insert_node("proxy.example", 8080, None, None)
        .await
        .unwrap();
    assert_ne!(node.enabled, 0, "insert defaults enabled");

    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/nodes/{}/toggle", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["id"], node.id);
    assert_eq!(v["enabled"], false);
    assert_eq!(v["host"], "proxy.example");
    assert_eq!(v["port"], 8080);

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/nodes/{}/toggle", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["enabled"], true);
}

#[tokio::test]
async fn toggle_node_requires_admin() {
    let db = test_db().await;
    let node = db
        .insert_node("proxy.example", 8080, None, None)
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/nodes/{}/toggle", node.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_request_logs_empty_then_after_insert() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v.as_array().expect("logs array").len(), 0);

    db.insert_request_log(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(15),
        None,
        Some("wave0 query"),
    )
    .await
    .unwrap();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/request-logs")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["path"], "/api/search");
    assert_eq!(row["method"], "POST");
    assert_eq!(row["status"], 200);
    assert_eq!(row["service"], "tavily");
    assert_eq!(row["providerUsed"], "tavily");
    assert_eq!(row["durationMs"], 15);
    assert_eq!(row["queryPreview"], "wave0 query");
    assert!(row["id"].as_i64().unwrap() > 0);
    assert!(row["createdAt"].is_string());
}

#[tokio::test]
async fn list_request_logs_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/request-logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
