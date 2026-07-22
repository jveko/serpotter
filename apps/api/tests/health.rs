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
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["expected"], 1);
}
