use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use serpotter_api::{app, AppState};
use serpotter_db::connect_and_migrate;
use serpotter_keypool::KeyPool;
use serpotter_tavily::TavilyClient;
use tower::ServiceExt;

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn state_with(db: serpotter_db::Db) -> AppState {
    AppState {
        keys: Arc::new(KeyPool::new(db.clone())),
        tavily: TavilyClient::new("http://127.0.0.1:9"), // unused on auth-only paths
        db,
    }
}

#[tokio::test]
async fn live_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["status"], "ok");
}

#[tokio::test]
async fn ready_ok_after_migrate() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["status"], "ok");
    assert_eq!(v["schemaVersion"], 3);
    assert_eq!(v["expected"], 3);
}

#[tokio::test]
async fn ready_not_ready_when_schema_stale() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    sqlx::query("UPDATE schema_version SET version = 0 WHERE id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["status"], "not_ready");
    assert_eq!(v["schemaVersion"], 0);
    assert_eq!(v["expected"], 3);
}

#[tokio::test]
async fn search_missing_token_is_401_problem_json() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
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
    let v = body_json(res).await;
    assert_eq!(v["status"], 401);
    assert_eq!(v["detail"], "Missing API token");
}

#[tokio::test]
async fn search_invalid_token_is_401() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));

    let res = app
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
    let v = body_json(res).await;
    assert_eq!(v["detail"], "Invalid token");
}

#[tokio::test]
async fn search_no_key_is_503() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", "Bearer tok-validtokenfortest0000000000000000")
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
async fn search_accepts_x_api_key_auth_header() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-xapikeytoken00000000000000000000", "x")
        .await
        .unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("x-api-key", "tok-xapikeytoken00000000000000000000")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // no tavily key → 503
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
}
