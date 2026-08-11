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

// --- F01: auth runs before body parsing --------------------------------------

/// No token + malformed JSON: the parts-level ApiToken extractor answers 401
/// before AppJson ever touches the body.
#[tokio::test]
async fn search_no_token_malformed_json_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "auth must win over body parse"
    );
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Authentication Error");
}

/// Bad token + malformed JSON: same 401-before-body ordering.
#[tokio::test]
async fn search_bad_token_malformed_json_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", "Bearer tok-does-not-exist")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Authentication Error");
}

/// Valid token + malformed JSON: auth passes, then AppJson maps the syntax
/// error to a 400 problem+json (not plain text).
#[tokio::test]
async fn search_valid_token_malformed_json_400_problem() {
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
                .body(Body::from(r#"{"query":"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "syntax error → 400");
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "rejection must be problem+json"
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Invalid Json");
    assert!(v["type"].as_str().unwrap_or("").ends_with("/InvalidJson"));
}

// --- F10: overall request deadline -------------------------------------------

/// A search blocked on an at-cap key pool exceeds the 1s request deadline:
/// the deadline fires before the 30s acquire timeout and answers
/// 504 RequestTimeout problem+json. Env is scoped to this test; concurrent
/// tests in this binary only observe a 1s deadline on requests that complete
/// in milliseconds (providers pinned to 127.0.0.1:9).
#[tokio::test]
async fn search_request_timeout_504() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-timeout").await.unwrap();
    std::env::set_var("REQUEST_TIMEOUT_SECS", "1");
    let st = state_with_key_pool(
        db.clone(),
        1,
        std::time::Duration::from_secs(30),
        serpotter_db::KEY_HOLD_TTL_SECS,
    );
    let _lease = st.keys.acquire("xai").await.expect("lease xai key");
    let app = app(st);
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
    std::env::remove_var("REQUEST_TIMEOUT_SECS");
    assert_eq!(
        res.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "deadline must fire"
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Request Timeout", "problem: {v}");
    assert!(v["type"]
        .as_str()
        .unwrap_or("")
        .ends_with("/RequestTimeout"));
    assert!(
        v["detail"].as_str().unwrap_or("").contains("deadline"),
        "detail names the deadline: {v}"
    );
}
