mod common;

use common::*;

#[tokio::test]
async fn create_token_returns_full_token_201() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tokens")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"web-ui"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let v = body_json(res).await;
    assert!(v["id"].as_i64().unwrap() > 0);
    assert_eq!(v["name"], "web-ui");
    let token = v["token"].as_str().expect("full token on create");
    assert!(token.starts_with("tok-"), "token prefix: {token}");
    assert_eq!(token.len(), 4 + 43, "tok- + base64url(32B) no pad");
    assert!(
        v.get("tokenPreview").is_none(),
        "create returns the full token, not a preview: {v}"
    );
    assert!(v["createdAt"].is_string());

    // Persisted and usable: the minted token authenticates a product route
    // (auth passes → reaches the key pool → 503 NoHealthyKey, never 401).
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "minted token must authenticate (503 = reached key pool)"
    );
}

#[tokio::test]
async fn create_token_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tokens")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"name":"web"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_tokens_masks_token_and_orders() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "first").await.unwrap();
    db.insert_token("tok-secondtoken00000000000000000", "second")
        .await
        .unwrap();
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/tokens")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("tokens array");
    assert_eq!(rows.len(), 2);
    // Ordered by id ASC; the fixture token was inserted first.
    assert_eq!(rows[0]["name"], "first");
    assert_eq!(rows[1]["name"], "second");
    for row in rows {
        assert!(
            row.get("token").is_none(),
            "list must never expose the full token: {row}"
        );
        let preview = row["tokenPreview"].as_str().expect("tokenPreview");
        assert!(preview.starts_with("tok-"), "masked preview: {preview}");
        assert!(row["createdAt"].is_string());
    }
}

#[tokio::test]
async fn list_tokens_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/tokens")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_token_204_then_404() {
    let db = test_db().await;
    let t = db.insert_token(TEST_TOKEN, "delete-me").await.unwrap();
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tokens/{}", t.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // Deleted token no longer authenticates.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Second delete of the same id is 404.
    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tokens/{}", t.id))
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
async fn delete_token_requires_admin() {
    let db = test_db().await;
    let t = db.insert_token(TEST_TOKEN, "del-auth").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tokens/{}", t.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
