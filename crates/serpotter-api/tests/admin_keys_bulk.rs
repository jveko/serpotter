mod common;

use common::*;

/// Bulk + per-service key-pool adds — the HTTP path for seeding many
/// provider keys at once (previously direct SQL inserts into the volume).
/// Mirrors admin_keys_crud.rs fixture/auth patterns.

#[tokio::test]
async fn bulk_add_keys_inserts_and_reports_per_key() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/bulk")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"service":"tavily","keys":["tvly-bulk-0001","tvly-bulk-0002","tvly-bulk-0003"]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert_eq!(v["service"], "tavily", "response: {v}");
    assert_eq!(v["inserted"], 3, "response: {v}");
    assert_eq!(v["skipped"], 0, "response: {v}");
    let results = v["results"].as_array().expect("results array");
    assert_eq!(results.len(), 3);
    for r in results {
        assert_eq!(r["status"], "inserted");
        assert!(r["keyPreview"].is_string(), "masked preview: {r}");
    }
    let raw = v.to_string();
    assert!(
        !raw.contains("tvly-bulk-0001"),
        "response must never leak raw keys: {raw}"
    );
    // Persisted: three rows now in the pool.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(res).await;
    assert_eq!(v.as_array().expect("keys array").len(), 3);
}

#[tokio::test]
async fn bulk_add_skips_duplicates_and_is_idempotent() {
    let db = test_db().await;
    db.insert_api_key("tavily", "tvly-preexisting-1")
        .await
        .unwrap();
    let app = app(state_with(db));
    let body = r#"{"service":"tavily","keys":["tvly-preexisting-1","tvly-fresh-2"]}"#;

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/bulk")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert_eq!(v["inserted"], 1, "only the fresh key lands: {v}");
    assert_eq!(v["skipped"], 1, "existing key skipped, not a 409: {v}");
    assert_eq!(v["results"][0]["status"], "skipped");
    assert_eq!(v["results"][1]["status"], "inserted");

    // Re-running the same import is a safe no-op (bulk import idempotency).
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/bulk")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert_eq!(v["inserted"], 0, "re-import inserts nothing: {v}");
    assert_eq!(v["skipped"], 2, "re-import skips everything: {v}");
}

#[tokio::test]
async fn bulk_add_validation_400() {
    let db = test_db().await;
    let app = app(state_with(db));
    let overlong = "x".repeat(300);
    let too_many = (0..1001)
        .map(|i| format!(r#""tvly-many-{i}""#))
        .collect::<Vec<_>>()
        .join(",");
    for body in [
        // Missing service on the bulk route.
        r#"{"keys":["tvly-x"]}"#.to_string(),
        // Unknown service.
        r#"{"service":"google","keys":["tvly-x"]}"#.to_string(),
        // Empty batch.
        r#"{"service":"tavily","keys":[]}"#.to_string(),
        // Blank entry inside the batch.
        r#"{"service":"tavily","keys":["tvly-ok","  "]}"#.to_string(),
        // Over-long entry (admin string bound).
        format!(r#"{{"service":"tavily","keys":["{overlong}"]}}"#),
        // Over the per-request batch cap.
        format!(r#"{{"service":"firecrawl","keys":[{too_many}]}}"#),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys/bulk")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
        let v = body_json(res).await;
        assert_eq!(v["status"], 400, "problem body: {v}");
    }
    // Nothing above may have landed in the pool.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(res).await;
    assert_eq!(
        v.as_array().expect("keys array").len(),
        0,
        "rejected batches must not partially insert: {v}"
    );
}

#[tokio::test]
async fn bulk_add_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    for uri in ["/api/keys/bulk", "/api/keys/tavily"] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"keys":["tvly-x"]}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "uri {uri}");
    }
}

#[tokio::test]
async fn per_service_routes_add_keys_for_every_provider() {
    let db = test_db().await;
    let app = app(state_with(db));
    for service in ["tavily", "firecrawl", "exa", "xai"] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/keys/{service}"))
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"keys":["{service}-pool-1","{service}-pool-2"]}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED, "service {service}");
        let v = body_json(res).await;
        assert_eq!(v["service"], service, "path fixes the service: {v}");
        assert_eq!(v["inserted"], 2, "response: {v}");
        assert_eq!(v["skipped"], 0, "response: {v}");
    }
    // Eight rows total across the four services.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(res).await;
    assert_eq!(v.as_array().expect("keys array").len(), 8);
}

#[tokio::test]
async fn per_service_route_validates_keys_and_ignores_body_service() {
    let db = test_db().await;
    let app = app(state_with(db));
    // Empty/blank batch rejected even via a per-service route.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/exa")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"keys":[]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // A service field in the body must NOT override the path.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/exa")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"service":"tavily","keys":["exa-path-wins-1"]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert_eq!(v["service"], "exa", "path wins over body: {v}");
}
