mod common;

use common::*;

/// Admin key CRUD (create/delete/toggle) — the primary mutation surface for
/// the key pool. Mirrors the admin_keys_credits.rs fixture/auth patterns.

#[tokio::test]
async fn create_key_returns_201_with_masked_preview() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"service":"firecrawl","key":"fc-0123456789abcdef0123"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert!(v["id"].as_i64().unwrap() > 0);
    assert_eq!(v["service"], "firecrawl");
    assert_eq!(v["keyPreview"], "fc-0…0123", "masked preview: {v}");
    assert_eq!(v["active"], true, "insert defaults active: {v}");
    assert_eq!(v["consecutiveFails"], 0);
    assert!(
        v.get("key").is_none(),
        "create response must never leak the raw api key: {v}"
    );
    // Persisted: list returns the new row.
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
    assert_eq!(rows[0]["service"], "firecrawl");
    assert!(rows[0]["key"].is_null(), "list must never leak: {rows:?}");
}

#[tokio::test]
async fn create_key_validation_400() {
    let db = test_db().await;
    let app = app(state_with(db));
    for body in [
        r#"{"service":"","key":"tvly-x"}"#,
        r#"{"service":"tavily","key":"  "}"#,
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/keys")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
        let v = body_json(res).await;
        assert_eq!(v["title"], "Validation Error", "problem body: {v}");
        assert_eq!(v["status"], 400);
    }
}

#[tokio::test]
async fn create_key_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"tavily","key":"tvly-x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_key_204_then_404() {
    let db = test_db().await;
    let k = db.insert_api_key("exa", "exa-key-to-delete").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/keys/{}", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Second delete of the same id is 404 (not a silent no-op).
    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/keys/{}", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Not Found");
}

#[tokio::test]
async fn delete_key_requires_admin() {
    let db = test_db().await;
    let k = db.insert_api_key("exa", "exa-del-auth").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/keys/{}", k.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn toggle_key_flips_active_and_persists() {
    let db = test_db().await;
    let k = db.insert_api_key("xai", "xai-toggle-me").await.unwrap();
    let app = app(state_with(db.clone()));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/keys/{}/toggle", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["id"], k.id);
    assert_eq!(v["active"], false, "KeyOut uses active field: {v}");
    assert_eq!(v["service"], "xai");

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 0, "toggle must persist active=0");

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/keys/{}/toggle", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["active"], true, "second toggle re-enables: {v}");
}

#[tokio::test]
async fn toggle_key_requires_admin() {
    let db = test_db().await;
    let k = db
        .insert_api_key("tavily", "tvly-toggle-auth")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/keys/{}/toggle", k.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn toggle_key_missing_404() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/keys/9999999/toggle")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_key_rotates_key_resets_fails_and_keeps_service() {
    let db = test_db().await;
    let k = db
        .insert_api_key("tavily", "tvly-old-key-1234567890")
        .await
        .unwrap();
    // Bump consecutive_fails to prove rotation clears it.
    db.report_api_key_failure(k.id).await.unwrap();
    db.report_api_key_failure(k.id).await.unwrap();
    let before = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(before.consecutive_fails, 2);

    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/keys/{}", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"key":"tvly-rotated-new-9999"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["id"], k.id);
    assert_eq!(v["service"], "tavily", "service untouched by key rotation");
    assert_eq!(v["keyPreview"], "tvly…9999", "masked rotated preview: {v}");
    assert_eq!(v["consecutiveFails"], 0, "rotation resets failure slate");
    assert!(
        v.get("key").is_none(),
        "update response must never leak the raw api key: {v}"
    );

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.key, "tvly-rotated-new-9999", "rotation persisted");
}

#[tokio::test]
async fn update_key_service_change_clears_credit_snapshot() {
    let db = test_db().await;
    let k = db
        .insert_api_key("tavily", "tvly-credits-12345")
        .await
        .unwrap();
    db.set_api_key_credits(k.id, Some(4321)).await.unwrap();
    let before = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(before.credits_remaining, Some(4321));

    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/keys/{}", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"service":"firecrawl"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["service"], "firecrawl");
    assert!(
        v.get("creditsRemaining").is_none() || v["creditsRemaining"].is_null(),
        "stale credit snapshot must be cleared on service change: {v}"
    );

    let after = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(after.service, "firecrawl");
    assert_eq!(
        after.credits_remaining, None,
        "credits cleared on service change"
    );
}

#[tokio::test]
async fn update_key_requires_at_least_one_field() {
    let db = test_db().await;
    let k = db.insert_api_key("exa", "exa-update-empty").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/keys/{}", k.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error");
}

#[tokio::test]
async fn update_key_rejects_blank_and_unknown_service() {
    let db = test_db().await;
    let k = db.insert_api_key("xai", "xai-update-svc").await.unwrap();
    let app = app(state_with(db));
    for body in [
        r#"{"service":"  "}"#,
        r#"{"key":"  "}"#,
        r#"{"service":"unknown-vendor"}"#,
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/keys/{}", k.id))
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "body {body}");
        let v = body_json(res).await;
        assert_eq!(v["title"], "Validation Error", "problem body: {v}");
        assert_eq!(v["status"], 400);
    }
}

#[tokio::test]
async fn update_key_missing_404_and_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));
    // Missing id with valid admin → 404.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/keys/9999999")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"key":"tvly-x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Not Found");

    // No admin auth → 401 even for a valid id.
    let k = db.insert_api_key("tavily", "tvly-upd-auth").await.unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/keys/{}", k.id))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"key":"tvly-x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
