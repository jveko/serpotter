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
