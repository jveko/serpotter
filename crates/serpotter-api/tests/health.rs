mod common;

use common::*;

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

#[tokio::test]
async fn ready_ok_schema_v10() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["status"], "ready");
    assert_eq!(v["schemaVersion"], 10);
    assert_eq!(v["expected"], 10);
}
