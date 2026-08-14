//! F56: server-minted request-id → request event correlation (no inbound
//! x-request-id), plus inbound-id truncation to [`MAX_REQUEST_ID_LEN`].
//!
//! The only pin today is tower-http's SetRequestIdLayer also inserting the
//! minted header (which `request_id_from_headers` reads); these tests make the
//! cross-layer contract explicit: the common curl/no-header case must produce
//! a 32-hex `request_id` ring row that matches the response header.

mod common;

use common::*;
use serpotter_api::trace_layer::MAX_REQUEST_ID_LEN;

/// Poll `/api/request-logs` until a row for `/api/search` appears (the ring
/// is fed synchronously by emit in the handler, so this is belt-and-braces).
async fn wait_for_search_ring_row(app: axum::Router) -> (Option<String>, i64) {
    for _ in 0..50 {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?path=%2Fapi%2Fsearch&limit=20")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if res.status() == StatusCode::OK {
            let v = body_json(res).await;
            if let Some(row) = v.as_array().and_then(|a| a.first()) {
                return (
                    row["requestId"].as_str().map(String::from),
                    row["status"].as_i64().unwrap_or(0),
                );
            }
        }
    }
    panic!("no /api/search ring row after poll window");
}

/// Seed a token + single xai key; providers pinned to 127.0.0.1:9 fail fast
/// with a 502 SearchError, which still emits a request event with the id.
#[tokio::test]
async fn minted_request_id_correlates_request_log_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-correlate").await.unwrap();
    let app = app(state_with(db.clone()));

    let res = app
        .clone()
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

    // The ring row for the same request carries the identical id.
    let (row_id, status) = wait_for_search_ring_row(app).await;
    assert_eq!(
        row_id.as_deref(),
        Some(echoed.as_str()),
        "request_log.request_id must equal the response's minted id"
    );
    assert_eq!(status, 502, "providers pinned at :9 → SearchError row");
}

/// Inbound x-request-id longer than 64 bytes is truncated before it reaches
/// the response header and the request event.
#[tokio::test]
async fn inbound_request_id_truncated_to_64_bytes() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("xai", "xai-trunc").await.unwrap();
    let app = app(state_with(db.clone()));

    let long = "x".repeat(200);
    let res = app
        .clone()
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

    let (row_id, _status) = wait_for_search_ring_row(app).await;
    assert_eq!(
        row_id.as_deref(),
        Some(echoed.as_str()),
        "request_log must observe the bounded id"
    );
}
