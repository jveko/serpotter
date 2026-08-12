//! B14: admin password rotation + session revocation integration tests.

mod common;

use common::*;

const INITIAL_PASSWORD: &str = "initial-pass-1";
const ROTATED_PASSWORD: &str = "rotated-pass-2";

/// Bootstrap the admin user (ADMIN_SECRET auth, default username "admin"),
/// then log in twice so two sessions exist. Returns (app, tokenA, tokenB) —
/// the app and sessions share one db, so later requests see the sessions.
async fn bootstrap_two_sessions(db: serpotter_db::Db) -> (axum::Router, String, String) {
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/bootstrap")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"password":"{INITIAL_PASSWORD}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED, "bootstrap must succeed");

    let login = |app: axum::Router, bearer: &str| {
        let app = app.clone();
        let bearer = bearer.to_owned();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/login")
                    .header("Authorization", format!("Bearer {bearer}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"username":"admin","password":"{INITIAL_PASSWORD}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // First login without any admin bearer (login itself takes the password).
    let res = login(app.clone(), "").await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let token_a = v["token"].as_str().expect("token A").to_string();

    let res = login(app.clone(), "").await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let token_b = v["token"].as_str().expect("token B").to_string();

    (app, token_a, token_b)
}

#[tokio::test]
async fn change_password_revokes_other_sessions_keeps_current() {
    let db = test_db().await;
    let (app, token_a, token_b) = bootstrap_two_sessions(db).await;

    // Both sessions are valid admin auth before the change.
    for token in [&token_a, &token_b] {
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
        assert_eq!(res.status(), StatusCode::OK, "session {token} valid");
    }

    // Change password as session A.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/change-password")
                .header("Authorization", format!("Bearer {token_a}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"currentPassword":"{INITIAL_PASSWORD}","newPassword":"{ROTATED_PASSWORD}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {token_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "other session revoked"
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {token_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "current session stays valid");

    // New password logs in; old password is rejected.
    let old_login = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"admin","password":"{INITIAL_PASSWORD}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old_login.status(), StatusCode::UNAUTHORIZED);

    let new_login = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/login")
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"username":"admin","password":"{ROTATED_PASSWORD}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(new_login.status(), StatusCode::OK);
}

#[tokio::test]
async fn change_password_wrong_current_returns_401() {
    let db = test_db().await;
    let (app, token_a, _token_b) = bootstrap_two_sessions(db).await;

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/change-password")
                .header("Authorization", format!("Bearer {token_a}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"currentPassword":"wrong-password","newPassword":"{ROTATED_PASSWORD}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Authentication Error");
}

#[tokio::test]
async fn change_password_short_new_password_returns_400() {
    let db = test_db().await;
    let (app, token_a, _token_b) = bootstrap_two_sessions(db).await;

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/change-password")
                .header("Authorization", format!("Bearer {token_a}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"currentPassword":"{INITIAL_PASSWORD}","newPassword":"short"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error");
}

#[tokio::test]
async fn change_password_same_password_returns_400() {
    let db = test_db().await;
    let (app, token_a, _token_b) = bootstrap_two_sessions(db).await;

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/change-password")
                .header("Authorization", format!("Bearer {token_a}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"currentPassword":"{INITIAL_PASSWORD}","newPassword":"{INITIAL_PASSWORD}"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sessions_list_marks_current_and_revoke_by_token() {
    let db = test_db().await;
    let (app, token_a, token_b) = bootstrap_two_sessions(db).await;

    // List as session A: two rows, A marked current.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/sessions")
                .header("Authorization", format!("Bearer {token_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("sessions array");
    assert_eq!(rows.len(), 2);
    let row_a = rows
        .iter()
        .find(|r| r["token"] == token_a)
        .expect("session A row");
    assert_eq!(row_a["current"], true);
    assert!(
        row_a["tokenPreview"].is_string(),
        "must carry a masked preview"
    );
    let row_b = rows
        .iter()
        .find(|r| r["token"] == token_b)
        .expect("session B row");
    assert_eq!(row_b["current"], false);

    // Revoke B by token → 204; unknown token → 404.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/admin/sessions/{token_b}"))
                .header("Authorization", format!("Bearer {token_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/admin/sessions/{token_b}"))
                .header("Authorization", format!("Bearer {token_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "second revoke is unknown"
    );

    // B is no longer an admin credential.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/stats")
                .header("Authorization", format!("Bearer {token_b}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
