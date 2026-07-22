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
    AppState {
        keys: Arc::new(KeyPool::new(db.clone())),
        providers: ProviderRegistry {
            tavily: TavilyClient::new("http://127.0.0.1:9"),
            firecrawl: FirecrawlClient::new("http://127.0.0.1:9"),
            exa: ExaClient::new("http://127.0.0.1:9"),
            xai: XaiClient::new("http://127.0.0.1:9"),
        },
        db,
        admin_secret: Some("test-admin-secret".into()),
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
async fn ready_ok_schema_v4() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["schemaVersion"], 4);
    assert_eq!(v["expected"], 4);
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

#[tokio::test]
async fn extract_missing_token_401() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn research_missing_query_400() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"  "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn admin_stats_with_secret() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", "Bearer test-admin-secret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["tokens"], 1);
    assert_eq!(v["schemaVersion"], 4);
}

#[tokio::test]
async fn admin_rejects_without_secret() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/api/stats").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_tools_list() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert!(v["result"]["tools"].as_array().unwrap().len() >= 4);
}

#[tokio::test]
async fn mcp_health_tool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_token("tok-validtokenfortest0000000000000000", "t")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header(
                    "Authorization",
                    "Bearer tok-validtokenfortest0000000000000000",
                )
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mysearch_health","arguments":{}}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], false);
}
