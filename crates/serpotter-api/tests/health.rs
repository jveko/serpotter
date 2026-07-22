use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use serpotter_api::{app, AppState};
use serpotter_db::connect_and_migrate;
use serpotter_keypool::KeyPool;
use serpotter_providers::{
    ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
};
use tower::ServiceExt;

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn state_with(db: serpotter_db::Db) -> AppState {
    // Point providers at unreachable localhost so auth/key-pool paths don't hit network.
    AppState {
        keys: Arc::new(KeyPool::new(db.clone())),
        providers: ProviderRegistry {
            tavily: TavilyClient::new("http://127.0.0.1:9"),
            firecrawl: FirecrawlClient::new("http://127.0.0.1:9"),
            exa: ExaClient::new("http://127.0.0.1:9"),
            xai: XaiClient::new("http://127.0.0.1:9"),
        },
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
}

#[tokio::test]
async fn ready_ok_schema_v3() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["schemaVersion"], 3);
    assert_eq!(v["expected"], 3);
}

#[tokio::test]
async fn search_missing_token_401() {
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
}

#[tokio::test]
async fn search_no_key_503() {
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
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
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
