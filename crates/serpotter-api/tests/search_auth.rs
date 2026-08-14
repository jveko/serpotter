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

/// F07: the documented x-api-key header path works at the HTTP layer.
#[tokio::test]
async fn search_x_api_key_header_authenticates() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("x-api-key", TEST_TOKEN)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Auth passed (no 401); no keys seeded → NoHealthyKey 503.
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["title"], "No Healthy Key");
}

/// F61: an invalid token via x-api-key is rejected 401 with problem+json.
#[tokio::test]
async fn search_invalid_x_api_key_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("x-api-key", "tok-invalidtoken0000000000000000")
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

// --- F08: 401 auth failures must be visible in request_log ------------------

/// Poll `/api/request-logs` for a row whose `requestId` matches `want`. The
/// poll is belt-and-braces — emission is synchronous, so the row is already
/// present by the time the response returns.
async fn poll_log_row_by_request_id(app: axum::Router, want: &str) -> Option<serde_json::Value> {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?limit=100")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "admin list must authorize");
        let v = body_json(res).await;
        if let Some(row) = v
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["requestId"] == want))
        {
            return Some(row.clone());
        }
    }
    None
}

/// A missing/absent token answers 401 AND writes a request_log row with
/// errorKind "Unauthorized" so failed auth attempts are auditable.
#[tokio::test]
async fn search_missing_token_401_logs_unauthenticated_row() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .clone()
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
    let request_id = res
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("response echoes x-request-id")
        .to_string();

    let row = poll_log_row_by_request_id(app.clone(), &request_id)
        .await
        .expect("failed auth must produce a request_log row");
    assert_eq!(row["status"], 401);
    assert_eq!(row["errorKind"], "Unauthorized");
    assert_eq!(row["path"], "/api/search");
    assert!(row["tokenName"].is_null(), "no token to attribute: {row}");
}

/// An invalid token also logs a 401 auth-failure row (same audit path).
#[tokio::test]
async fn search_invalid_token_401_logs_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", "Bearer tok-does-not-exist")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let request_id = res
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("response echoes x-request-id")
        .to_string();

    let row = poll_log_row_by_request_id(app.clone(), &request_id)
        .await
        .expect("invalid-token auth failure must be logged");
    assert_eq!(row["status"], 401);
    assert_eq!(row["errorKind"], "Unauthorized");
    assert!(row["tokenName"].is_null());
}

// --- FU10: REST closed-set validation matches the MCP boundary --------------

/// A typo'd routing knob is rejected 400 ValidationError, not silently coerced.
#[tokio::test]
async fn search_invalid_strategy_400() {
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
                .body(Body::from(r#"{"query":"hello","strategy":"bogus"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error", "problem: {v}");
    assert!(
        v["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/ValidationError"),
        "type: {v}"
    );
    assert!(
        v["detail"].as_str().unwrap_or("").contains("strategy"),
        "detail names the field: {v}"
    );
}

/// An unknown provider is rejected 400 (same set as MCP).
#[tokio::test]
async fn search_invalid_provider_400() {
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
                .body(Body::from(r#"{"query":"hello","provider":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error");
}
