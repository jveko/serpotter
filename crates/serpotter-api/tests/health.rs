mod common;

use common::*;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

#[tokio::test]
async fn live_ok() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

/// Mirrors the main.rs layer stack (Propagate inner → trace → Set outer):
/// every response must carry an x-request-id, minted once by SetRequestIdLayer.
#[tokio::test]
async fn live_sets_request_id_header() {
    let db = test_db().await;
    let app = app(state_with(db))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(serpotter_api::trace_layer::make_trace_layer())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));
    let res = app
        .oneshot(Request::builder().uri("/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let rid = res
        .headers()
        .get("x-request-id")
        .expect("x-request-id response header");
    assert!(!rid.is_empty(), "x-request-id must not be empty");
}

#[tokio::test]
async fn ready_ok_schema_v12() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["status"], "ready");
    assert_eq!(v["schemaVersion"], 12);
    assert_eq!(v["expected"], 12);
}
