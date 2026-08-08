mod common;

use common::*;

#[tokio::test]
async fn search_missing_token_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
}

#[tokio::test]
async fn search_no_key_503() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["title"], "No Healthy Key");
}

#[tokio::test]
async fn search_key_busy_503() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // xai fallback_chain is only ["xai"] — no secondary NoHealthyKey overwrite.
    let _k = db.insert_api_key("xai", "xai-busy-hold").await.unwrap();
    let held = db
        .acquire_api_key_shared(
            "xai",
            1,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap();
    assert!(held.is_some(), "pre-hold must succeed");

    let app = app(state_with_key_pool(
        db,
        1,
        std::time::Duration::from_millis(80),
        serpotter_db::KEY_HOLD_TTL_SECS,
    ));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello","provider":"xai"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Key Busy", "problem: {v}");
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/KeyBusy"),
        "type uri: {v}"
    );
}

#[tokio::test]
async fn search_provider_http_maps_to_search_error() {
    // Key present, provider base is 127.0.0.1:9 → connection refused → SearchError 502.
    // Use xai so fallback_chain is single-provider (no empty-inventory overwrite).
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-search-err").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello","provider":"xai"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_GATEWAY,
        "expected SearchError path"
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Search Error", "problem: {v}");
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/SearchError"),
        "type uri: {v}"
    );
}
