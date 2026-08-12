//! OpenAI-compatible /v1 surface (B4): auth, model routing, one-shot + SSE
//! streaming, request_log (`request_mode`/`ttft_ms`), F10 deadline wrap.
//!
//! Providers are pinned at 127.0.0.1:9 (connection refused), so the product
//! always fails fast; the success-path response assembly (one-shot JSON,
//! stream deltas, usage blocks) is unit-tested in `src/v1/chat.rs`. This
//! suite proves the HTTP contract: auth 401, unknown model 404, error
//! mapping, SSE error frame + `[DONE]`, and the request_log row carrying
//! `request_mode=stream` + `ttft_ms >= 0`.

mod common;

use common::*;

fn chat_body(model: &str, messages: serde_json::Value) -> String {
    serde_json::json!({ "model": model, "messages": messages }).to_string()
}

fn user_msg(text: &str) -> serde_json::Value {
    serde_json::json!([{ "role": "user", "content": text }])
}

fn v1_request(body: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    if let Some(tok) = token {
        builder = builder.header("Authorization", format!("Bearer {tok}"));
    }
    builder.body(Body::from(body.to_string())).unwrap()
}

/// Poll the request_log table (direct DB, not the admin DTO — `ttft_ms` and
/// `request_mode` are columns the admin JSON omits) for a row with the wanted
/// request id.
async fn poll_db_log_row(db: serpotter_db::Db, want: &str) -> Option<serpotter_db::RequestLogRow> {
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let rows = db
            .list_request_logs(serpotter_db::RequestLogFilter {
                limit: 100,
                offset: 0,
                status: None,
                path_prefix: None,
                service: None,
                request_id: Some(want.to_string()),
                token_name: None,
            })
            .await
            .expect("list request_logs");
        if let Some(row) = rows
            .into_iter()
            .find(|r| r.request_id.as_deref() == Some(want))
        {
            return Some(row);
        }
    }
    None
}

#[tokio::test]
async fn v1_chat_missing_token_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(v1_request(
            &chat_body("serpotter-search", user_msg("hello")),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json")
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Authentication Error");
}

#[tokio::test]
async fn v1_models_missing_token_401() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

/// GET /v1/models lists the valid model set (search/research + the xAI alias).
#[tokio::test]
async fn v1_models_lists_valid_set() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/models")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["object"], "list");
    let ids: Vec<&str> = v["data"]
        .as_array()
        .expect("data array")
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"serpotter-search") && ids.contains(&"serpotter-research"),
        "model set: {ids:?}"
    );
    assert!(ids.contains(&"grok-4.5"), "xAI alias present: {ids:?}");
    for m in v["data"].as_array().unwrap() {
        assert_eq!(m["object"], "model");
    }
}

#[tokio::test]
async fn v1_unknown_model_404() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(v1_request(
            &chat_body("banana", user_msg("hello")),
            Some(TEST_TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Unknown Model", "problem: {v}");
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/UnknownModel"),
        "type: {v}"
    );
    let detail = v["detail"].as_str().unwrap_or("");
    assert!(detail.contains("serpotter-search"), "detail: {v}");
    assert!(detail.contains("grok-4.5"), "detail: {v}");
}

#[tokio::test]
async fn v1_missing_user_message_400() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(v1_request(
            &chat_body(
                "serpotter-search",
                serde_json::json!([{ "role": "system", "content": "be brief" }]),
            ),
            Some(TEST_TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Validation Error");
}

/// One-shot search with no keys → NoHealthyKey 503 (proves the one-shot HTTP
/// path and the request_log row: request_mode=oneshot, strategy=search).
#[tokio::test]
async fn v1_one_shot_search_no_key_503_and_logs() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db.clone()));
    let res = app
        .clone()
        .oneshot(v1_request(
            &chat_body("serpotter-search", user_msg("hello")),
            Some(TEST_TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let request_id = res
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("response echoes x-request-id")
        .to_string();
    let v = body_json(res).await;
    assert_eq!(v["title"], "No Healthy Key", "problem: {v}");
    let row = poll_db_log_row(db, &request_id)
        .await
        .expect("one-shot must produce a request_log row");
    assert_eq!(row.path, "/v1/chat/completions");
    assert_eq!(row.status, 503);
    assert_eq!(row.strategy.as_deref(), Some("search"));
    assert_eq!(row.request_mode.as_deref(), Some("oneshot"));
    assert_eq!(row.ttft_ms, None, "one-shot leaves ttft_ms NULL");
}

/// One-shot search with a key (provider base :9) → connection refused →
/// 502 SearchError problem. The default fallback chain is
/// [tavily, exa, firecrawl] and the LAST provider error wins, so seeding the
/// firecrawl key (chain tail) makes the exhausting error the HTTP failure
/// rather than firecrawl's "no key".
#[tokio::test]
async fn v1_one_shot_search_error_502() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("firecrawl", "fc-v1").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(v1_request(
            &chat_body("serpotter-search", user_msg("hello")),
            Some(TEST_TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::BAD_GATEWAY,
        "provider connection refused maps to 502"
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Search Error", "problem: {v}");
}

/// Direct xAI path: model grok-4.5 with an xai key, provider at :9 →
/// 502 ProviderError problem.
#[tokio::test]
async fn v1_direct_xai_error_502() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-v1").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(v1_request(
            &chat_body("grok-4.5", user_msg("hello")),
            Some(TEST_TOKEN),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Provider Error", "problem: {v}");
    assert!(
        v["type"].as_str().unwrap_or("").ends_with("/ProviderError"),
        "type: {v}"
    );
}

/// Direct xAI path under the F10 deadline: holding the only xai key makes the
/// 30s acquire wait exceed the 1s request deadline → 504 RequestTimeout.
#[tokio::test]
async fn v1_direct_timeout_504() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-v1-timeout").await.unwrap();
    std::env::set_var("REQUEST_TIMEOUT_SECS", "1");
    let st = state_with_key_pool(
        db.clone(),
        1,
        std::time::Duration::from_secs(30),
        serpotter_db::KEY_HOLD_TTL_SECS,
    );
    let _lease = st.keys.acquire("xai").await.expect("lease xai key");
    let app = app(st);
    let res = app
        .oneshot(v1_request(
            &chat_body("grok-4.5", user_msg("hello")),
            Some(TEST_TOKEN),
        ))
        .await
        .unwrap();
    std::env::remove_var("REQUEST_TIMEOUT_SECS");
    assert_eq!(
        res.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "deadline must fire"
    );
    let v = body_json(res).await;
    assert_eq!(v["title"], "Request Timeout", "problem: {v}");
    assert!(v["detail"].as_str().unwrap_or("").contains("deadline"));
}

/// Streaming with a FAILING search: the stream must still emit SSE — a
/// `data: {"error": …}` frame + `data: [DONE]` — and log a request_log row
/// with `request_mode=stream`, `ttft_ms >= 0` and the mapped status/kind.
#[tokio::test]
async fn v1_stream_failing_search_emits_error_sse_and_logs() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // Firecrawl is the tail of the default web chain [tavily, exa, firecrawl];
    // seeding its key makes the exhausting error the HTTP failure (502
    // SearchError), not a sibling provider's missing key.
    db.insert_api_key("firecrawl", "fc-v1-stream")
        .await
        .unwrap();
    let app = app(state_with(db.clone()));
    let body = serde_json::json!({
        "model": "serpotter-search",
        "messages": [{ "role": "user", "content": "hello" }],
        "stream": true,
    })
    .to_string();
    let res = app
        .clone()
        .oneshot(v1_request(&body, Some(TEST_TOKEN)))
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "stream never 4xx/5xx at HTTP level"
    );
    let ct = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let request_id = res
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .expect("response echoes x-request-id")
        .to_string();
    assert!(
        ct.starts_with("text/event-stream"),
        "content-type must be text/event-stream, got {ct}"
    );
    let text = String::from_utf8(body_bytes(res).await.to_vec()).unwrap();
    assert!(
        text.contains("\"error\"") && text.contains("\"SearchError\""),
        "error frame must carry the mapped kind: {text}"
    );
    assert!(
        text.contains("\"status\":502") || text.contains("\"status\": 502"),
        "error frame carries status: {text}"
    );
    assert!(
        text.contains("data: [DONE]"),
        "stream must end with [DONE]: {text}"
    );

    let row = poll_db_log_row(db, &request_id)
        .await
        .expect("stream must produce a request_log row");
    assert_eq!(row.path, "/v1/chat/completions");
    assert_eq!(row.status, 502, "status from search_problem mapping");
    assert_eq!(row.strategy.as_deref(), Some("search"));
    assert_eq!(row.request_mode.as_deref(), Some("stream"));
    assert!(
        row.ttft_ms.is_some_and(|t| t >= 0.0),
        "stream ttft_ms captured: {row:?}"
    );
}
