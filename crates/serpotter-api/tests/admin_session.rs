mod common;

use common::*;

#[tokio::test]
async fn admin_stats_with_secret() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["tokens"], 1);
    assert_eq!(v["schemaVersion"], 20);
    assert_eq!(v["recentRequests"], 0);
    assert!(v["byService"].is_array());
}

#[tokio::test]
async fn admin_rejects_without_secret() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_bootstrap_login_session_protects_stats() {
    let db = test_db().await;
    let app = app(state_with(db));

    // bootstrap requires ADMIN_SECRET
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/bootstrap")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"password":"s3cret-pass"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let boot = body_json(res).await;
    assert_eq!(boot["username"], "admin");

    // second bootstrap conflicts
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/bootstrap")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"password":"other"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CONFLICT);

    // login
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"admin","password":"s3cret-pass"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let login = body_json(res).await;
    let token = login["token"].as_str().expect("token");
    assert!(token.starts_with("adm-"));
    assert!(login["expiresAt"].is_string());

    // session protects stats
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["schemaVersion"], 20);

    // logout
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/logout")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // session no longer valid
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_secret_still_works_without_sessions() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_settings_durable_roundtrip() {
    let db = test_db().await;
    let app = app(state_with(db));

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/settings")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"socialEnabled":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let put_v = body_json(put).await;
    assert_eq!(put_v["socialEnabled"], false);
    // note must not claim in-memory stub
    if let Some(n) = put_v.get("note").and_then(|x| x.as_str()) {
        assert!(!n.contains("in-memory"));
    }

    let get = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/settings")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let get_v = body_json(get).await;
    assert_eq!(get_v["socialEnabled"], false);
}

/// `get_valid_admin_session` filters on `expires_at > datetime('now')`, and
/// `require_admin` turns "no valid row" into 401. Only the DB side of that
/// pair was pinned (`serpotter-db/tests/migrate.rs`); this seeds an expired
/// session directly — the shape the 7-day TTL promise produces the moment
/// a token ages out — and proves the HTTP gate refuses it.
#[tokio::test]
async fn admin_expired_session_bearer_401_over_http() {
    let db = test_db().await;
    let user = db
        .insert_admin_user("admin", "$argon2id$placeholder")
        .await
        .unwrap();
    db.insert_admin_session("adm-expired", user.id, "2000-01-01 00:00:00")
        .await
        .unwrap();
    // The row is really there (only its expiry is in the past), so the 401
    // below is the expiry filter refusing a stored credential — not a
    // token that was never issued. Whether the filter itself excludes it is
    // serpotter-db's contract (tests/migrate.rs), not this layer's.
    assert!(db
        .list_admin_sessions()
        .await
        .unwrap()
        .iter()
        .any(|s| s.token == "adm-expired"));

    let app = app(state_with(db));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", "Bearer adm-expired")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "expired session must not authorize"
    );
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
    );
    let v = body_json(res).await;
    assert_eq!(v["status"], 401, "problem: {v}");
    assert_eq!(
        v["type"],
        "https://serpotter.dev/errors/AuthenticationError"
    );

    // Same expiry refusal on the metrics surface, which shares the gate.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .header("Authorization", "Bearer adm-expired")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
    );
}
