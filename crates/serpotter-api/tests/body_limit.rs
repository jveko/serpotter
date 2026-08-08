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