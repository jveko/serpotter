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
    assert_eq!(v["schemaVersion"], 11);
    assert_eq!(v["requestLogs"], 0);
    assert!(v["byService"].is_array());
}

#[tokio::test]
async fn admin_rejects_without_secret() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(Request::builder().uri("/api/stats").body(Body::empty()).unwrap())
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
    assert_eq!(v["schemaVersion"], 11);

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
