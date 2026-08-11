//! Body-limit contract: requests larger than `BODY_LIMIT_BYTES` are rejected
//! with 413 Payload Too Large by the `DefaultBodyLimit` layer before any
//! handler (and its admin auth) runs.

mod common;

use common::*;
use serpotter_api::BODY_LIMIT_BYTES;

/// A request with a `Content-Length` over `BODY_LIMIT_BYTES` is rejected with
/// 413 Payload Too Large. tower-http's `RequestBodyLimit` (what
/// `DefaultBodyLimit::max` installs) short-circuits on the header before the
/// body is read or the handler is reached.
#[tokio::test]
async fn oversized_body_is_413() {
    let db = test_db().await;
    let app = app(state_with(db));

    // Honest oversized payload: a valid admin create-key JSON body padded well
    // past the 2 MiB limit.
    let payload = format!(
        r#"{{"service":"tavily","key":"{}"}}"#,
        "k".repeat(BODY_LIMIT_BYTES + 128 * 1024)
    );
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("host", "localhost")
                .header("content-type", "application/json")
                .header("x-admin-password", TEST_ADMIN_SECRET)
                .header("content-length", payload.len().to_string())
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();

    // Observed contract (tower-http 0.6 RequestBodyLimit): over-limit
    // Content-Length -> immediate 413, body "length limit exceeded".
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

/// The same route, admin-authenticated, accepts a normal-sized body — proving
/// the 413 above came from the body limit, not from the route or auth.
#[tokio::test]
async fn under_limit_body_passes_through() {
    let db = test_db().await;
    let app = app(state_with(db));

    let payload = r#"{"service":"tavily","key":"tvly-test-key-1234567890"}"#;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("content-type", "application/json")
                .header("x-admin-password", TEST_ADMIN_SECRET)
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
}

// --- F00: every product-body rejection is problem+json -----------------------
// The three product handlers use AppJson, which maps each axum Json rejection
// to the same RFC 9457 shape as handler errors (stable kind + problem
// content-type), so no rejection path leaks a plain-text body.

fn problem_content_type(res: &axum::response::Response) -> Option<&str> {
    res.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
}

/// `{}` is valid JSON but misses the required `query` field → 422 InvalidJson
/// problem+json (axum's JsonDataError).
#[tokio::test]
async fn search_empty_object_is_422_problem_json() {
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
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "missing field → 422"
    );
    assert_eq!(problem_content_type(&res), Some("application/problem+json"));
    let v = body_json(res).await;
    assert_eq!(v["title"], "Invalid Json");
    assert!(v["type"].as_str().unwrap_or("").ends_with("/InvalidJson"));
}

/// Malformed JSON → 400 InvalidJson problem+json (axum's JsonSyntaxError).
#[tokio::test]
async fn search_malformed_json_is_400_problem_json() {
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
    assert_eq!(problem_content_type(&res), Some("application/problem+json"));
    let v = body_json(res).await;
    assert_eq!(v["title"], "Invalid Json");
    assert!(
        v["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "detail carries the parse error: {v}"
    );
}

/// No JSON content-type → 415 InvalidContentType problem+json.
#[tokio::test]
async fn search_missing_content_type_is_415_problem_json() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "missing content-type → 415"
    );
    assert_eq!(problem_content_type(&res), Some("application/problem+json"));
    let v = body_json(res).await;
    assert_eq!(v["title"], "Invalid Content Type");
}

/// Body over `BODY_LIMIT_BYTES` → 413 BodyTooLarge problem+json (the Bytes
/// extractor's LengthLimitError, mapped by AppJson).
#[tokio::test]
async fn search_oversized_body_is_413_problem_json() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    let payload = format!(
        r#"{{"query":"{}"}}"#,
        "k".repeat(BODY_LIMIT_BYTES + 128 * 1024)
    );
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("content-length", payload.len().to_string())
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "over-limit → 413"
    );
    assert_eq!(problem_content_type(&res), Some("application/problem+json"));
    let v = body_json(res).await;
    assert_eq!(v["title"], "Body Too Large");
    assert!(v["type"].as_str().unwrap_or("").ends_with("/BodyTooLarge"));
}
