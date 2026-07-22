mod common;

use common::*;

#[tokio::test]
async fn sync_credits_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/sync-credits")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn sync_credits_empty_keys_ok() {
    let db = test_db().await;
    // Providers point at 127.0.0.1:9; empty key list avoids network.
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/sync-credits")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["service"], "tavily");
    assert_eq!(v["synced"], 0);
    assert_eq!(v["errors"], 0);
    assert_eq!(v["results"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn sync_credits_fetch_fail_keeps_key_active() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-soft-fail").await.unwrap();
    // Providers point at 127.0.0.1:9 → connection refused → soft error, not deactivate.
    let app = app(state_with(db.clone()));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/sync-credits")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["synced"], 0);
    assert!(v["errors"].as_i64().unwrap() >= 1);
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1, "fetch fail must not set active=0");
}
