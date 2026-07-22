use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use serpotter_api::{app, AppState};
use serpotter_db::connect_and_migrate;
use tower::ServiceExt;

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn live_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(AppState { db });

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
    let app = app(AppState { db });

    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["status"], "ok");
    assert_eq!(v["schemaVersion"], 2);
    assert_eq!(v["expected"], 2);
}

#[tokio::test]
async fn ready_not_ready_when_schema_stale() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    sqlx::query("UPDATE schema_version SET version = 0 WHERE id = 1")
        .execute(db.pool())
        .await
        .unwrap();
    let app = app(AppState { db });

    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["status"], "not_ready");
    assert_eq!(v["schemaVersion"], 0);
    assert_eq!(v["expected"], 2);
}

#[tokio::test]
async fn search_missing_token_is_401_problem_json() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(AppState { db });

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .body(Body::empty())
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
    assert_eq!(v["title"], "Authentication Error");
    assert_eq!(v["detail"], "Missing API token");
    assert_eq!(
        v["type"],
        "https://serpotter.dev/errors/AuthenticationError"
    );
}

#[tokio::test]
async fn search_invalid_token_is_401() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(AppState { db });

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", "Bearer tok-does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let v = body_json(res).await;
    assert_eq!(v["detail"], "Invalid token");
}

#[tokio::test]
async fn search_valid_token_is_501_stub() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(AppState { db });

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", "Bearer tok-validtokenfortest0000000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    let v = body_json(res).await;
    assert_eq!(v["status"], "not_implemented");
}

#[tokio::test]
async fn search_accepts_x_api_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-xapikeytoken00000000000000000000", "x")
        .await
        .unwrap();
    let app = app(AppState { db });

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("x-api-key", "tok-xapikeytoken00000000000000000000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
}
