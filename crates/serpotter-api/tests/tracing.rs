//! F56: server-minted request-id → request_log correlation (no inbound
//! x-request-id), plus inbound-id truncation to [`MAX_REQUEST_ID_LEN`].
//!
//! The only pin today is tower-http's SetRequestIdLayer also inserting the
//! minted header (which `request_id_from_headers` reads); these tests make the
//! cross-layer contract explicit: the common curl/no-header case must produce
//! a 32-hex `request_id` row that matches the response header.

mod common;

use common::*;
use serpotter_api::trace_layer::MAX_REQUEST_ID_LEN;

/// Poll the DB until a request_log row for `/api/search` appears (spawn_log is
/// fire-and-forget). Returns its `request_id` + `status`.
async fn wait_for_search_log_row(db: &serpotter_db::Db) -> (Option<String>, i64) {
    for _ in 0..50 {
        let row: Option<(Option<String>, i64)> = sqlx::query_as(
            "SELECT request_id, status FROM request_log \
             WHERE path = '/api/search' ORDER BY id DESC LIMIT 1",
        )
        .fetch_optional(db.pool())
        .await
        .expect("query request_log");
        if let Some(row) = row {
            return row;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("no /api/search request_log row after poll window");
}

/// Seed a token + single xai key; providers pinned to 127.0.0.1:9 fail fast
/// with a 502 SearchError, which still logs a request_log row with the id.
#[tokio::test]
async fn minted_request_id_correlates_request_log_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-correlate").await.unwrap();
    let app = app(state_with(db.clone()));

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"query":"hello","provider":"xai"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::BAD_GATEWAY,
        "providers pinned at :9 → 502"
    );

    // Response echoes the server-minted id (PropagateRequestIdLayer).
    let echoed = res
        .headers()
        .get("x-request-id")
        .expect("response must carry x-request-id")
        .to_str()
        .expect("ascii id")
        .to_string();
    assert_eq!(
        echoed.len(),
        32,
        "server-minted ids are 32 hex chars: {echoed}"
    );
    assert!(
        echoed.chars().all(|c| c.is_ascii_hexdigit()),
        "minted id must be hex: {echoed}"
    );

    // The request_log row for the same request carries the identical id.
    let (row_id, status) = wait_for_search_log_row(&db).await;
    assert_eq!(
        row_id.as_deref(),
        Some(echoed.as_str()),
        "request_log.request_id must equal the response's minted id"
    );
    assert_eq!(status, 502, "providers pinned at :9 → SearchError row");
}

/// Inbound x-request-id longer than 64 bytes is truncated before it reaches
/// the response header and the request_log row.
#[tokio::test]
async fn inbound_request_id_truncated_to_64_bytes() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-trunc").await.unwrap();
    let app = app(state_with(db.clone()));

    let long = "x".repeat(200);
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("x-request-id", &long)
                .body(Body::from(r#"{"query":"hello","provider":"xai"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::BAD_GATEWAY,
        "providers pinned at :9 → 502"
    );

    let echoed = res
        .headers()
        .get("x-request-id")
        .expect("response must carry x-request-id")
        .to_str()
        .expect("ascii id")
        .to_string();
    assert_eq!(
        echoed.len(),
        MAX_REQUEST_ID_LEN,
        "oversized inbound id must be truncated to {MAX_REQUEST_ID_LEN} bytes"
    );
    assert_eq!(echoed, &long[..64], "truncation keeps the first 64 bytes");

    let (row_id, _status) = wait_for_search_log_row(&db).await;
    assert_eq!(
        row_id.as_deref(),
        Some(echoed.as_str()),
        "request_log must observe the bounded id"
    );
}
