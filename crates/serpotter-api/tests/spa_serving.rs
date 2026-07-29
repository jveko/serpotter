mod common;

use common::*;
use serpotter_api::app_with_spa;

/// Minimal built-SPA shape: index.html + a hashed-asset stand-in.
const SPA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/spa");

async fn get(uri: &str) -> axum::response::Response {
    let db = test_db().await;
    app_with_spa(state_with(db), Some(SPA_DIR))
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn text(res: axum::response::Response) -> String {
    String::from_utf8(body_bytes(res).await.to_vec()).unwrap()
}

#[tokio::test]
async fn root_serves_index() {
    let res = get("/").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(text(res).await.contains("id=\"root\""));
}

/// The reported bug: refreshing on a client route used to 404 because ServeDir
/// looked for a *file* named `keys`. It must boot the SPA instead.
#[tokio::test]
async fn client_route_refresh_serves_index() {
    for uri in ["/keys", "/logs", "/playground", "/login"] {
        let res = get(uri).await;
        assert_eq!(res.status(), StatusCode::OK, "GET {uri}");
        assert!(
            text(res).await.contains("id=\"root\""),
            "GET {uri} should fall back to index.html"
        );
    }
}

#[tokio::test]
async fn real_assets_are_served_not_index() {
    let res = get("/assets/app.js").await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = text(res).await;
    assert!(
        body.contains("spa-asset"),
        "expected the asset, got: {body}"
    );
}

/// The SPA fallback must never shadow the API. A mistyped endpoint has to answer
/// a JSON problem, not HTML with 200.
#[tokio::test]
async fn unknown_api_path_is_json_404() {
    for uri in ["/api", "/api/nope", "/api/tokens/1/bogus"] {
        let res = get(uri).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "GET {uri}");
        let body = text(res).await;
        assert!(
            !body.contains("id=\"root\""),
            "GET {uri} must not return the SPA shell"
        );
    }
}

#[tokio::test]
async fn health_routes_win_over_spa_fallback() {
    let res = get("/live").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["status"], "ok");

    let res = get("/ready").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["status"], "ready");
}

/// `app()` still wires every route after being split over `app_with_spa`.
#[tokio::test]
async fn env_driven_app_still_routes() {
    let db = test_db().await;
    let res = app(state_with(db))
        .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn no_spa_configured_leaves_root_unrouted() {
    let db = test_db().await;
    let res = app_with_spa(state_with(db), None)
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
