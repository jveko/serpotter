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

#[tokio::test]
async fn list_keys_returns_credits_and_inflight() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-list-credits").await.unwrap();
    db.set_api_key_credits(k.id, Some(42)).await.unwrap();
    db.update_api_key_usage(k.id, 42, 100).await.unwrap();

    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("keys array");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"], k.id);
    assert_eq!(row["service"], "tavily");
    assert_eq!(row["creditsRemaining"], 42);
    assert_eq!(row["creditsLimit"], 100);
    assert!(row["usageSyncedAt"].is_string());
    assert_eq!(row["inflight"], 0);
    assert!(row["active"].as_bool().unwrap());
    // leaseUntil omitted when null (skip_serializing_if); force a value and re-list.
    assert!(
        row.get("leaseUntil").is_none() || row["leaseUntil"].is_null(),
        "idle key should not advertise leaseUntil: {row}"
    );
    assert!(row.get("key").is_none(), "must not leak full api_key");
}

#[tokio::test]
async fn list_keys_returns_lease_until_when_set() {
    let db = test_db().await;
    let k = db
        .insert_api_key("tavily", "tvly-lease-until")
        .await
        .unwrap();
    db.set_api_key_lease_until(k.id, Some("2099-01-01 00:00:00"))
        .await
        .unwrap();

    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let row = &v.as_array().expect("keys array")[0];
    assert_eq!(row["id"], k.id);
    assert_eq!(
        row["leaseUntil"].as_str(),
        Some("2099-01-01 00:00:00"),
        "KeyOut must surface leaseUntil: {row}"
    );
    assert!(row.get("key").is_none(), "must not leak full api_key");
}

#[tokio::test]
async fn list_keys_returns_last_used_at_when_set() {
    let db = test_db().await;
    let k = db
        .insert_api_key("tavily", "tvly-last-used")
        .await
        .unwrap();
    db.set_api_key_last_used_at(k.id, Some("2099-02-02 12:00:00"))
        .await
        .unwrap();

    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let row = &v.as_array().expect("keys array")[0];
    assert_eq!(row["id"], k.id);
    assert_eq!(
        row["lastUsedAt"].as_str(),
        Some("2099-02-02 12:00:00"),
        "KeyOut must surface lastUsedAt: {row}"
    );
    assert!(row.get("key").is_none(), "must not leak full api_key");
}
