mod common;

use common::*;

#[tokio::test]
async fn extract_missing_token_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"https://example.com"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn extract_ssrf_localhost_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"url":"http://localhost/secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error");
}

#[tokio::test]
async fn research_missing_query_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"  "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn research_request_accepts_web_max_results_aliases() {
    // mysearch REST: webMaxResults / scrapeTopN
    let body = r#"{"query":"q","webMaxResults":3,"scrapeTopN":1}"#;
    let req: serpotter_api::ResearchRequest = serde_json::from_str(body).unwrap();
    assert_eq!(req.web_max_results, Some(3));
    assert_eq!(req.scrape_top_n, Some(1));

    let body2 = r#"{"query":"q","maxResults":4,"extractTopN":2}"#;
    let req2: serpotter_api::ResearchRequest = serde_json::from_str(body2).unwrap();
    assert_eq!(req2.web_max_results, Some(4));
    assert_eq!(req2.scrape_top_n, Some(2));
}

#[tokio::test]
async fn research_success_body_has_web_results_key() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // no api keys → 503 NoHealthyKey before body; seed a fake key so search attempts provider and fails open?
    // With no keys: 503 — still assert we don't return old {search, extracts} shape on success path.
    // Seed key so routing runs; provider points at 127.0.0.1:9 → 502 ProviderError, not research success.
    // For success-shaped body without network: unit-test ResearchResponse serde instead.
    let sample = serpotter_api::ResearchResponse {
        query: "q".into(),
        web_results: vec![],
        social_results: None,
        social_error: None,
        scraped_pages: Some(vec![]),
        citations: None,
        evidence: None,
    };
    let v = serde_json::to_value(&sample).unwrap();
    assert!(
        v.get("webResults").is_some(),
        "expected camelCase webResults: {v}"
    );
    assert!(
        v.get("scrapedPages").is_some(),
        "expected scrapedPages: {v}"
    );
    assert!(v.get("search").is_none());
    assert!(v.get("extracts").is_none());

    // HTTP path with token + empty query already covered; with valid query and no keys → 503
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"query":"hello","webMaxResults":3,"scrapeTopN":1}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    // no keys → 503; body is problem+json, not research success
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert_eq!(v["title"], "No Healthy Key");
}

#[test]
fn research_response_serializes_social_results_when_some() {
    let sample = serpotter_api::ResearchResponse {
        query: "q".into(),
        web_results: vec![],
        social_results: Some(vec![]),
        social_error: None,
        scraped_pages: None,
        citations: None,
        evidence: None,
    };
    let v = serde_json::to_value(&sample).unwrap();
    assert!(v.get("socialResults").is_some());
    assert_eq!(v["socialResults"].as_array().unwrap().len(), 0);
}

/// Empty query returns 400 AND a request_log row with status=400,
/// errorKind=ValidationError, echoed requestId, token name, and null
/// key_id/node_id (validation never touches keys/nodes).
#[tokio::test]
async fn research_missing_query_logs_validation_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "validation-test")
        .await
        .unwrap();
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("x-request-id", "val-req-1")
                .body(Body::from(r#"{"query":"  "}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // spawn_log is fire-and-forget — poll until the row lands.
    let mut found = None;
    for _ in 0..50 {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?path=/api/research&limit=20")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let v = body_json(res).await;
        let rows = v.as_array().expect("logs array");
        if let Some(row) = rows.iter().find(|r| r["path"] == "/api/research") {
            found = Some(row.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let row = found.expect("expected /api/research request_log row after 400");
    assert_eq!(row["status"], 400, "validation row status: {row}");
    assert_eq!(
        row["errorKind"], "ValidationError",
        "validation row errorKind: {row}"
    );
    assert_eq!(row["requestId"], "val-req-1", "echoed x-request-id: {row}");
    assert_eq!(
        row["tokenName"], "validation-test",
        "token name from TEST token: {row}"
    );
    assert!(row["keyId"].is_null(), "key_id must be null: {row}");
    assert!(row["nodeId"].is_null(), "node_id must be null: {row}");
}
