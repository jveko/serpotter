//! B6: usage dashboard integration tests — /api/usage, /api/spend/keys,
//! /api/spend/services (seeded request_log → rollup → admin endpoints).

mod common;

use common::*;

/// Seed request_log rows via the db insert path (all columns; new token/cost
/// columns appended per the 0015 DDL contract), then run the usage rollup the
/// way the maintenance cron would.
async fn seed_and_rollup(db: &serpotter_db::Db) -> (i64, i64) {
    let tavily_key = db
        .insert_api_key("tavily", "tvly-usage-test-key")
        .await
        .unwrap();
    let firecrawl_key = db
        .insert_api_key("firecrawl", "fc-usage-test-key")
        .await
        .unwrap();

    db.insert_request_log_full(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(12),
        None,
        Some("usage q1"),
        Some("usage-req-1"),
        Some("tok-a"),
        Some("hybrid"),
        Some("tavily,firecrawl"),
        Some(1),
        Some(tavily_key.id),
        None,
        Some(80),
        Some(20),
        Some(100),
        Some(0.5),
    )
    .await
    .unwrap();

    // Same service/provider, failed request: counted as an error with tokens.
    db.insert_request_log_full(
        "/api/search",
        "POST",
        502,
        Some("tavily"),
        Some("tavily"),
        Some(9),
        Some("ProviderError"),
        Some("usage q2"),
        Some("usage-req-2"),
        Some("tok-a"),
        Some("hybrid"),
        Some("tavily"),
        Some(2),
        Some(tavily_key.id),
        None,
        None,
        None,
        None,
        Some(0.5),
    )
    .await
    .unwrap();

    db.insert_request_log_full(
        "/api/extract",
        "POST",
        200,
        Some("firecrawl"),
        Some("firecrawl"),
        Some(44),
        None,
        Some("https://example.com"),
        Some("usage-req-3"),
        Some("tok-a"),
        Some("single"),
        Some("firecrawl"),
        Some(1),
        Some(firecrawl_key.id),
        None,
        Some(150),
        Some(50),
        Some(200),
        Some(1.0),
    )
    .await
    .unwrap();

    let rolled = db
        .rollup_usage_from_request_log(24 * 7)
        .await
        .expect("rollup");
    assert_eq!(
        rolled, 2,
        "rollup upserts one usage_daily group per (service, provider_used, date): \
         (tavily,tavily) and (firecrawl,firecrawl)"
    );
    (tavily_key.id, firecrawl_key.id)
}

#[tokio::test]
async fn usage_summary_reflects_seeded_rollup() {
    let db = test_db().await;
    seed_and_rollup(&db).await;
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/usage?days=14")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("usage rows array");
    assert!(!rows.is_empty(), "expected rolled-up usage rows");

    let tavily = rows
        .iter()
        .find(|r| r["service"] == "tavily")
        .expect("tavily row");
    assert_eq!(tavily["providerUsed"], "tavily");
    assert_eq!(tavily["requests"], 2);
    assert_eq!(tavily["successes"], 1);
    assert_eq!(tavily["errors"], 1);
    assert_eq!(tavily["tokens"], 100);
    assert!(
        (tavily["cost"].as_f64().unwrap() - 1.0).abs() < 1e-9,
        "tavily cost 0.5+0.5 = 1.0, got {:?}",
        tavily["cost"]
    );
    assert!(tavily["date"].is_string(), "date must be present");

    let firecrawl = rows
        .iter()
        .find(|r| r["service"] == "firecrawl")
        .expect("firecrawl row");
    assert_eq!(firecrawl["requests"], 1);
    assert_eq!(firecrawl["successes"], 1);
    assert_eq!(firecrawl["errors"], 0);
    assert_eq!(firecrawl["tokens"], 200);
    assert!((firecrawl["cost"].as_f64().unwrap() - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn usage_defaults_to_14_days_and_clamps() {
    let db = test_db().await;
    seed_and_rollup(&db).await;
    let app = app(state_with(db));

    // Default (no days param) and explicit 14 must both work.
    for url in ["/api/usage", "/api/usage?days=14"] {
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
        assert!(v.as_array().is_some(), "url {url} must return an array");
    }

    // Out-of-range clamps (90 max, 1 min) instead of erroring.
    for url in [
        "/api/usage?days=999",
        "/api/usage?days=0",
        "/api/usage?days=-5",
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
        assert_eq!(res.status(), StatusCode::OK, "clamped url {url}");
    }
}

#[tokio::test]
async fn usage_requires_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn spend_by_keys_groups_cost_per_key_with_service() {
    let db = test_db().await;
    let (tavily_id, _fc_id) = seed_and_rollup(&db).await;
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/spend/keys")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("spend keys array");
    assert!(!rows.is_empty());

    let key_row = rows
        .iter()
        .find(|r| r["keyId"] == tavily_id)
        .expect("tavily key row");
    assert_eq!(key_row["service"], "tavily");
    assert_eq!(key_row["requests"], 2);
    assert!(
        (key_row["cost"].as_f64().unwrap() - 1.0).abs() < 1e-9,
        "tavily key cost = 1.0, got {:?}",
        key_row["cost"]
    );
}

#[tokio::test]
async fn spend_by_services_groups_cost_per_service() {
    let db = test_db().await;
    seed_and_rollup(&db).await;
    let app = app(state_with(db));

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/spend/services")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    let rows = v.as_array().expect("spend services array");

    let tavily = rows
        .iter()
        .find(|r| r["service"] == "tavily")
        .expect("tavily service row");
    assert_eq!(tavily["requests"], 2);
    assert!((tavily["cost"].as_f64().unwrap() - 1.0).abs() < 1e-9);

    let firecrawl = rows
        .iter()
        .find(|r| r["service"] == "firecrawl")
        .expect("firecrawl service row");
    assert_eq!(firecrawl["requests"], 1);
    assert!((firecrawl["cost"].as_f64().unwrap() - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn spend_endpoints_require_admin() {
    let db = test_db().await;
    let app = app(state_with(db));
    for url in ["/api/spend/keys", "/api/spend/services"] {
        let res = app
            .clone()
            .oneshot(Request::builder().uri(url).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "url {url}");
    }
}
