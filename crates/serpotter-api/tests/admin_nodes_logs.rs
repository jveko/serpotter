mod common;

use common::*;

#[tokio::test]
async fn toggle_node_flips_enabled() {
    let db = test_db().await;
    let node = db
        .insert_node("proxy.example", 8080, None, None, "http")
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
        .insert_node("proxy.example", 8080, None, None, "http")
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
async fn list_nodes_returns_last_error_when_set() {
    let db = test_db().await;
    let node = db
        .insert_node("err.example", 3128, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(node.id, 5, Some("tunnel timeout"))
        .await
        .unwrap();

    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/nodes")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("nodes array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], node.id);
    assert_eq!(rows[0]["lastError"], "tunnel timeout");
    assert_eq!(rows[0]["consecutiveFails"], 1);
}

#[tokio::test]
async fn toggle_node_reenable_clears_fails_and_last_error() {
    let db = test_db().await;
    let node = db
        .insert_node("dead.example", 8080, None, None, "http")
        .await
        .unwrap();
    for msg in ["a", "b", "c"] {
        db.acquire_outbound_node().await.unwrap().unwrap();
        db.report_node_failure(node.id, 3, Some(msg)).await.unwrap();
    }
    let dead = db.get_node(node.id).await.unwrap().unwrap();
    assert_eq!(dead.enabled, 0);
    assert_eq!(dead.consecutive_fails, 3);

    let app = app(state_with(db));
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
    assert_eq!(v["consecutiveFails"], 0);
    assert!(
        v.get("lastError").is_none() || v["lastError"].is_null(),
        "re-enable must clear lastError, got {:?}",
        v.get("lastError")
    );
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
