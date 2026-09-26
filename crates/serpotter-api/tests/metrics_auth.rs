//! `/metrics` auth gate: the exposition is an admin surface, so every way in
//! is pinned at the HTTP layer (the handler's own unit tests never call
//! `scrape_metrics`).
//!
//! `require_admin` accepts, in order: a valid unexpired session Bearer, the
//! `ADMIN_SECRET` as a Bearer, then the `X-Admin-Password` header. All
//! three are pinned here; the session's expiry refusal lives in
//! `admin_session.rs`.

mod common;

use axum::http::header::CONTENT_TYPE;
use common::*;

/// The gate is the same 401 every admin route answers: problem+json, never
/// an empty body or a redirect, so a scraper can tell "forbidden" from
/// "misconfigured".
#[tokio::test]
async fn metrics_unauthenticated_401_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "gate must answer problem+json"
    );
    let v = body_json(res).await;
    assert_eq!(v["status"], 401, "problem: {v}");
    assert_eq!(v["title"], "Authentication Error", "problem: {v}");
    assert_eq!(
        v["type"], "https://serpotter.dev/errors/AuthenticationError",
        "problem: {v}"
    );
}

/// A valid unexpired session authorizes `/metrics` — `require_admin`'s first
/// branch, and the only credential that still works when `ADMIN_SECRET` is
/// unset, so this is the scrape path a password-rotated deployment relies on.
#[tokio::test]
async fn metrics_valid_session_bearer_200_text_plain() {
    let db = test_db().await;
    let user = db
        .insert_admin_user("admin", "$argon2id$placeholder")
        .await
        .unwrap();
    db.insert_admin_session("adm-live", user.id, "2099-01-01 00:00:00")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("Authorization", "Bearer adm-live")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8"),
        "exposition content-type is part of the scrape contract"
    );
    let body = String::from_utf8(body_bytes(res).await.to_vec()).expect("utf-8");
    assert!(body.contains("serpotter_requests_in_flight"));
}

/// ADMIN_SECRET as a Bearer buys the Prometheus text exposition — the exact
/// content-type a scraper configures against.
#[tokio::test]
async fn metrics_admin_secret_bearer_200_text_plain() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8"),
        "exposition content-type is part of the scrape contract"
    );
    let body = String::from_utf8(body_bytes(res).await.to_vec()).expect("utf-8");
    assert!(
        body.contains("serpotter_requests_in_flight"),
        "exposition carries this module's registry: {body}"
    );
}

/// The second accepted credential header: same 200, same exposition.
#[tokio::test]
async fn metrics_admin_password_header_200_text_plain() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("x-admin-password", TEST_ADMIN_SECRET)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/plain; version=0.0.4; charset=utf-8"),
    );
    let body = String::from_utf8(body_bytes(res).await.to_vec()).expect("utf-8");
    assert!(body.contains("serpotter_requests_in_flight"));
}

/// A wrong credential is refused exactly like no credential — the gate does
/// not degrade into a 200 with an empty body.
#[tokio::test]
async fn metrics_wrong_bearer_401_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("Authorization", "Bearer not-the-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
    );
    let v = body_json(res).await;
    assert_eq!(
        v["type"],
        "https://serpotter.dev/errors/AuthenticationError"
    );
}
