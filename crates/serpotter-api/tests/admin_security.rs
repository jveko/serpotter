mod common;

use axum::extract::ConnectInfo;
use common::*;
use std::net::SocketAddr;

const PASSWORD: &str = "correct-pass-1";

async fn bootstrap_admin(app: &axum::Router) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/bootstrap")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"password":"{PASSWORD}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
}

fn login_request(username: &str, password: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/admin/login")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"username":"{username}","password":"{password}"}}"#
        )))
        .unwrap()
}

#[tokio::test]
async fn admin_bearer_scheme_is_case_insensitive() {
    let db = test_db().await;
    let app = app(state_with(db));

    for scheme in ["bearer", "BEARER"] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .header("Authorization", format!("{scheme} {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "scheme {scheme} must authorize"
        );
    }
}

#[tokio::test]
async fn bootstrap_rejects_password_under_eight_bytes_without_creating_user() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/bootstrap")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"password":"1234567"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert_eq!(db.count_admin_users().await.unwrap(), 0);
}

#[tokio::test]
async fn login_failures_are_indistinguishable_and_share_limit_with_bootstrap() {
    let db = test_db().await;
    let app = app(state_with(db));
    bootstrap_admin(&app).await;

    // Throttle identity is the client IP only: NAT co-location intentionally
    // shares a bucket, so each section below uses a distinct address.
    let mut unknown = login_request("does-not-exist", "wrong-password");
    unknown
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 10], 40000))));
    let mut known = login_request("admin", "wrong-password");
    known
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 11], 40001))));
    let unknown = app.clone().oneshot(unknown).await.unwrap();
    let known = app.clone().oneshot(known).await.unwrap();

    assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(known.status(), unknown.status());
    assert_eq!(
        known.headers().get("content-type"),
        unknown.headers().get("content-type")
    );
    assert_eq!(body_bytes(known).await, body_bytes(unknown).await);

    let invalid_bootstrap = |app: &axum::Router| {
        let app = app.clone();
        async move {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/admin/bootstrap")
                .header("Authorization", "Bearer wrong-admin-secret")
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"password":"{PASSWORD}"}}"#)))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 12], 40020))));
            app.oneshot(request).await.unwrap()
        }
    };

    assert_eq!(
        invalid_bootstrap(&app).await.status(),
        StatusCode::UNAUTHORIZED
    );
    for _ in 0..9 {
        let mut request = login_request("admin", "wrong-password");
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 12], 40021))));
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    // The nine failed logins above fill the 192.0.2.12 bucket; the next request
    // from it is rate limited before credentials are evaluated.
    let mut request = login_request("admin", "wrong-password");
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 12], 40022))));
    let res = app.clone().oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn logout_with_unknown_session_is_idempotent_no_content() {
    let db = test_db().await;
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/admin/logout")
                .header("Authorization", "Bearer adm-unknown-session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn admin_disabled_bootstrap_never_counts_client_failures() {
    let db = test_db().await;
    let state = state_without_admin_secret(db.clone());
    let app = app(state);

    for _ in 0..11 {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/admin/bootstrap")
            .header("content-type", "application/json")
            .body(Body::from(format!(r#"{{"password":"{PASSWORD}"}}"#)))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 40], 40050))));
        let res = app.clone().oneshot(request).await.unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(db.count_admin_users().await.unwrap(), 0);
}

#[tokio::test]
async fn logout_database_failure_returns_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));
    let mut bootstrap = Request::builder()
        .method("POST")
        .uri("/api/admin/bootstrap")
        .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"password":"{PASSWORD}"}}"#)))
        .unwrap();
    bootstrap
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 30], 40010))));
    assert_eq!(
        app.clone().oneshot(bootstrap).await.unwrap().status(),
        StatusCode::CREATED
    );

    let mut login = login_request("admin", PASSWORD);
    login
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([192, 0, 2, 31], 40011))));
    let login = app.clone().oneshot(login).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let token = body_json(login).await["token"]
        .as_str()
        .expect("session token")
        .to_string();

    db.pool().close().await;
    let res = app
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

    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/problem+json"
    );
    let body = body_json(res).await;
    assert_eq!(body["title"], "Database Error");
}
