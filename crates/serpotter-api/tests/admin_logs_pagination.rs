//! B13: request-log pagination + tokenName filter integration tests.

mod common;

use common::*;

/// Insert `n` request_log rows with the given token_name; request ids are
/// `page-{i}` so ordering assertions stay deterministic (newest = highest id).
async fn seed_logs(db: &serpotter_db::Db, n: i64, token_name: &str) {
    for i in 0..n {
        db.insert_request_log_full(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            Some("tavily"),
            Some(5),
            None,
            Some("page query"),
            Some(&format!("page-{i}")),
            Some(token_name),
            Some("hybrid"),
            Some("tavily"),
            Some(1),
            None,
            None,
            Some(10),
            Some(5),
            Some(15),
            Some(0.1),
            None,
            Some("oneshot"),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn pagination_walks_newest_first_pages() {
    let db = test_db().await;
    seed_logs(&db, 5, "tok-page-a").await;
    let app = app(state_with(db));

    let page = |url: &str| {
        let app = app.clone();
        let url = url.to_owned();
        async move {
            app.oneshot(
                Request::builder()
                    .uri(&url)
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // Page 1 (offset 0): the two newest rows.
    let res = page("/api/request-logs?limit=2&offset=0").await;
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["requestId"], "page-4", "newest first");
    assert_eq!(rows[1]["requestId"], "page-3");

    // Page 2 (offset 2): the next two.
    let res = page("/api/request-logs?limit=2&offset=2").await;
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["requestId"], "page-2");
    assert_eq!(rows[1]["requestId"], "page-1");

    // Page 3 (offset 4): the last one.
    let res = page("/api/request-logs?limit=2&offset=4").await;
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["requestId"], "page-0");

    // Past the end: empty page.
    let res = page("/api/request-logs?limit=2&offset=6").await;
    let v = body_json(res).await;
    assert_eq!(v.as_array().expect("logs array").len(), 0);
}

#[tokio::test]
async fn offset_defaults_to_zero_and_negative_clamps() {
    let db = test_db().await;
    seed_logs(&db, 3, "tok-offset").await;
    let app = app(state_with(db));

    for url in [
        "/api/request-logs?limit=2",
        "/api/request-logs?limit=2&offset=-5",
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(url)
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "url {url}");
        let v = body_json(res).await;
        let rows = v.as_array().expect("logs array");
        assert_eq!(rows.len(), 2, "url {url} must behave as page 0");
        assert_eq!(rows[0]["requestId"], "page-2");
    }
}

#[tokio::test]
async fn token_name_filter_matches_exactly() {
    let db = test_db().await;
    seed_logs(&db, 2, "tok-a").await;
    seed_logs(&db, 3, "tok-b").await;
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?tokenName=tok-b&limit=10")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|r| r["tokenName"] == "tok-b"),
        "exact tokenName filter must not match tok-a rows"
    );

    // Unknown token → empty list, not an error.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?tokenName=nope&limit=10")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v.as_array().expect("logs array").len(), 0);
}

#[tokio::test]
async fn token_name_filter_combines_with_pagination() {
    let db = test_db().await;
    seed_logs(&db, 4, "tok-combo").await;
    seed_logs(&db, 1, "tok-other").await;
    let app = app(state_with(db));

    // Newest-first among tok-combo rows only: page-3, page-2, page-1, page-0.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?tokenName=tok-combo&limit=2&offset=2")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(res).await;
    let rows = v.as_array().expect("logs array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["requestId"], "page-1");
    assert_eq!(rows[1]["requestId"], "page-0");
}
