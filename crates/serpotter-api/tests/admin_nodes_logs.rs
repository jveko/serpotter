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
        None,
        None,
        None,
        None,
        None,
        None,
        None,
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
async fn list_request_logs_observability_fields_and_filters() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));

    db.insert_request_log(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("hybrid"),
        Some(42),
        None,
        Some("hybrid query"),
        Some("req-obs-1"),
        Some("local-token"),
        Some("hybrid"),
        Some("tavily,firecrawl"),
        Some(2),
        Some(7),
        Some(3),
    )
    .await
    .unwrap();
    db.insert_request_log(
        "/api/extract",
        "POST",
        502,
        Some("firecrawl"),
        Some("firecrawl"),
        Some(9),
        Some("ProviderError"),
        Some("https://x"),
        Some("req-obs-2"),
        Some("other"),
        Some("single"),
        Some("firecrawl"),
        Some(1),
        Some(8),
        None,
    )
    .await
    .unwrap();

    // Full list: newest first — extract then search
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?limit=10")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 2);
    let hybrid = rows
        .iter()
        .find(|r| r["requestId"] == "req-obs-1")
        .expect("hybrid row");
    assert_eq!(hybrid["path"], "/api/search");
    assert_eq!(hybrid["service"], "tavily");
    assert_eq!(hybrid["providerUsed"], "hybrid");
    assert_eq!(hybrid["tokenName"], "local-token");
    assert_eq!(hybrid["strategy"], "hybrid");
    assert_eq!(hybrid["providersConsulted"], "tavily,firecrawl");
    assert_eq!(hybrid["attemptCount"], 2);
    assert_eq!(hybrid["keyId"], 7);
    assert_eq!(hybrid["nodeId"], 3);
    assert_eq!(hybrid["requestId"], "req-obs-1");

    // path prefix + requestId filters
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?path=/api/se&requestId=req-obs-1")
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
    assert_eq!(rows[0]["requestId"], "req-obs-1");
    assert_eq!(rows[0]["path"], "/api/search");

    // service filter
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?service=firecrawl")
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
    assert_eq!(rows[0]["service"], "firecrawl");
    assert_eq!(rows[0]["status"], 502);

    // status filter
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?status=200")
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
    assert_eq!(rows[0]["status"], 200);
}

/// Lenient status filter: `?status=2xx` (non-numeric) must return 200 with
/// unfiltered rows instead of a 400, so dashboard pass-throughs never break.
#[tokio::test]
async fn list_request_logs_status_lenient_string() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));

    db.insert_request_log(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(15),
        None,
        Some("lenient query"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?status=2xx")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "unparseable status must not 400"
    );
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 1, "filter treated as absent");
    assert_eq!(rows[0]["status"], 200);
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

#[tokio::test]
async fn create_node_default_protocol_http() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/nodes")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"host":"p.example","port":8080}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert_eq!(v["host"], "p.example");
    assert_eq!(v["port"], 8080);
    assert_eq!(v["protocol"], "http");
    assert_eq!(v["enabled"], true);
}

#[tokio::test]
async fn create_node_socks5_ok() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/nodes")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"s.example","port":1080,"protocol":"socks5"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert_eq!(v["protocol"], "socks5");
    assert_eq!(v["host"], "s.example");
    assert_eq!(v["port"], 1080);
}

#[tokio::test]
async fn create_node_bad_protocol_400() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/nodes")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"host":"x.example","port":1,"protocol":"ftp"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    let title = v["title"].as_str().unwrap_or("");
    assert!(
        title.contains("Validation") || v["type"].as_str().unwrap_or("").contains("Validation"),
        "expected ValidationError problem+json, got {v}"
    );
}

#[tokio::test]
async fn list_nodes_includes_protocol() {
    let db = test_db().await;
    let node = db
        .insert_node("list.example", 9, None, None, "https")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/nodes")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let row = v
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == node.id)
        .expect("node in list");
    assert_eq!(row["protocol"], "https");
}

#[tokio::test]
async fn update_node_patches_host_and_protocol() {
    let db = test_db().await;
    let node = db
        .insert_node("old.example", 8080, None, None, "http")
        .await
        .unwrap();
    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/nodes/{}", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"host":"new.example","protocol":"https"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["id"], node.id);
    assert_eq!(v["host"], "new.example");
    assert_eq!(v["port"], 8080, "unspecified fields keep their value");
    assert_eq!(v["protocol"], "https");
    assert_eq!(v["enabled"], true, "enabled state never touched");

    let row = db.get_node(node.id).await.unwrap().unwrap();
    assert_eq!(row.host, "new.example");
    assert_eq!(row.protocol, "https");
    assert_eq!(row.port, 8080);
}

#[tokio::test]
async fn update_node_clear_and_set_credentials() {
    let db = test_db().await;
    let node = db
        .insert_node("auth.example", 3128, Some("user1"), Some("pass1"), "http")
        .await
        .unwrap();
    let app = app(state_with(db.clone()));

    // Explicit null clears the stored credential pair.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/nodes/{}", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":null,"password":null}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(
        v.get("username").is_none() || v["username"].is_null(),
        "username must be cleared: {v}"
    );
    assert!(
        v.get("password").is_none() || v["password"].is_null(),
        "password must be cleared: {v}"
    );
    let row = db.get_node(node.id).await.unwrap().unwrap();
    assert_eq!(row.username, None);
    assert_eq!(row.password, None);

    // Setting one credential leaves the other untouched.
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/nodes/{}", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"user2"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["username"], "user2");
    let row = db.get_node(node.id).await.unwrap().unwrap();
    assert_eq!(row.username.as_deref(), Some("user2"));
    assert_eq!(row.password, None, "password stays cleared");
}

#[tokio::test]
async fn update_node_requires_at_least_one_field() {
    let db = test_db().await;
    let node = db
        .insert_node("empty.example", 8080, None, None, "http")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/nodes/{}", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error");
}

#[tokio::test]
async fn update_node_rejects_bad_port_blank_host_and_protocol() {
    let db = test_db().await;
    let node = db
        .insert_node("bad.example", 8080, None, None, "http")
        .await
        .unwrap();
    let app = app(state_with(db));
    for body in [
        r#"{"port":0}"#,
        r#"{"port":-5}"#,
        r#"{"host":"  "}"#,
        r#"{"protocol":"ftp"}"#,
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/nodes/{}", node.id))
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
        let v = body_json(res).await;
        assert_eq!(v["title"], "Validation Error", "problem body: {v}");
        assert_eq!(v["status"], 400);
    }
}

#[tokio::test]
async fn update_node_missing_404_and_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/nodes/9999999")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"host":"x.example"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Not Found");

    let node = db
        .insert_node("auth.example", 8080, None, None, "http")
        .await
        .unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/nodes/{}", node.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"host":"y.example"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
