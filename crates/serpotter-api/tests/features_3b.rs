//! Wave 3B surface tests: batch/question/highlights extract (B26/B27),
//! research backend + citation validation (B17/B31), exa deep search
//! routing (B20/B29), outputSchema (B28).

mod common;

use common::*;

// --- B28: exa deep search routing (provider=exa + strategy=deep) --------

/// provider=exa + strategy=deep routes to the exa deep leg; without an exa
/// key it fails NoHealthyKey 503 deterministically (no network).
#[tokio::test]
async fn search_exa_deep_without_key_is_503_no_healthy_key() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"query":"rust","provider":"exa","strategy":"deep"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/NoHealthyKey"),
        "{v}"
    );
}

/// The deep modes are valid search_depth values (MCP + REST closed set).
#[tokio::test]
async fn search_deep_depth_is_valid_and_routes_to_deep_leg() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"query":"rust","provider":"exa","searchDepth":"deep-lite"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/NoHealthyKey"),
        "deep-lite must not be a 400 validation error: {v}"
    );
}

/// provider=exa + outputSchema routes to the deep leg too.
#[tokio::test]
async fn search_output_schema_with_exa_routes_deep() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"query":"rust","provider":"exa","outputSchema":{"type":"object"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/NoHealthyKey"),
        "{v}"
    );
}

// --- B26/B27: extract batch / question / highlights on the REST wire -----

/// format=bogus is a client error (400), never a provider 5xx.
#[tokio::test]
async fn extract_bad_format_is_400() {
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
                .body(Body::from(
                    r#"{"url":"https://example.com","format":"bogus"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/ValidationError"),
        "{v}"
    );
}

/// format=question with provider=tavily is gated to firecrawl → 400.
#[tokio::test]
async fn extract_question_with_tavily_provider_is_400() {
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
                .body(Body::from(
                    r#"{"url":"https://example.com","format":"question","question":"what?","provider":"tavily"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["status"], 400, "{v}");
}

/// format=question without a question is a client error.
#[tokio::test]
async fn extract_question_without_question_is_400() {
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
                .body(Body::from(
                    r#"{"url":"https://example.com","format":"question"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["status"], 400, "{v}");
}

/// format=highlights with provider=tavily is gated to exa → 400.
#[tokio::test]
async fn extract_highlights_with_tavily_provider_is_400() {
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
                .body(Body::from(
                    r#"{"url":"https://example.com","format":"highlights","provider":"tavily"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["status"], 400, "{v}");
}

/// Batch urls + question/highlights is a client error (single-URL modes).
#[tokio::test]
async fn extract_batch_with_question_format_is_400() {
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
                .body(Body::from(
                    r#"{"url":"https://a.example","urls":["https://a.example","https://b.example"],"format":"question","question":"what?"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["status"], 400, "{v}");
}

/// Batch with provider=firecrawl (no batch backend) is a 400 client error.
#[tokio::test]
async fn extract_batch_with_firecrawl_provider_is_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("firecrawl", "fc-batch-400")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"url":"https://a.example","urls":["https://a.example","https://b.example"],"provider":"firecrawl"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["status"], 400, "{v}");
}

/// Batch with a tavily key: providers :9 → 502 ProviderError (dispatch ran).
#[tokio::test]
async fn extract_batch_tavily_provider_failure_maps_502() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-batch-test")
        .await
        .unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extract")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"url":"https://a.example","urls":["https://a.example","https://b.example"]}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let v = body_json(res).await;
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/ProviderError"),
        "{v}"
    );
}

// --- B17/B31: research backend + citation format validation --------------

#[tokio::test]
async fn research_unknown_backend_is_400() {
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
                .body(Body::from(r#"{"query":"x","researchBackend":"bogus"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/ValidationError"),
        "{v}"
    );
}

#[tokio::test]
async fn research_bad_citation_format_is_400() {
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
                .body(Body::from(
                    r#"{"query":"x","researchBackend":"tavily","citationFormat":"bogus"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["status"], 400, "{v}");
}

/// research_backend=tavily without a tavily key → NoHealthyKey 503
/// (deterministic — the acquire happens before any vendor call).
#[tokio::test]
async fn research_tavily_backend_without_key_is_503() {
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
                .body(Body::from(r#"{"query":"x","researchBackend":"tavily"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v = body_json(res).await;
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/NoHealthyKey"),
        "{v}"
    );
}

// --- B28: wire DTO shapes (unit) ------------------------------------------

/// SearchQuery outputSchema acceptance is exercised through the HTTP boundary
/// (search_output_schema_with_exa_routes_deep: provider=exa + outputSchema
/// must deserialize and route to the deep leg, never a 400).
///
/// ExtractRequest accepts urls/format/question/outputSchema.
#[test]
fn extract_request_accepts_wave3b_fields() {
    let req: serpotter_api::ExtractRequest = serde_json::from_str(
        r#"{"url":"https://a.example","urls":["https://a.example","https://b.example"],"format":"highlights","question":"q","outputSchema":{"type":"object"}}"#,
    )
    .expect("wave-3b extract fields deserialize");
    assert_eq!(
        req.urls.as_deref(),
        Some(
            &[
                "https://a.example".to_string(),
                "https://b.example".to_string()
            ][..]
        )
    );
    assert_eq!(req.format.as_deref(), Some("highlights"));
    assert_eq!(req.question.as_deref(), Some("q"));
    assert!(req.output_schema.is_some());
}

/// ResearchRequest accepts researchBackend/citationFormat/outputSchema.
#[test]
fn research_request_accepts_wave3b_fields() {
    let req: serpotter_api::ResearchRequest = serde_json::from_str(
        r#"{"query":"x","researchBackend":"tavily","citationFormat":"mla","outputSchema":{"type":"object"}}"#,
    )
    .expect("wave-3b research fields deserialize");
    assert_eq!(req.research_backend.as_deref(), Some("tavily"));
    assert_eq!(req.citation_format.as_deref(), Some("mla"));
    assert!(req.output_schema.is_some());
}

/// ExtractResponse carries the additive `pages` list on the wire.
#[test]
fn extract_response_serializes_pages() {
    let v: serde_json::Value = serde_json::from_str(
        r#"{"url":"https://a.example","content":"a","providerUsed":"tavily","pages":[{"url":"https://a.example","content":"a"}]}"#,
    )
    .unwrap();
    assert_eq!(v["pages"][0]["url"], "https://a.example");
    assert_eq!(v["pages"][0]["content"], "a");
}
