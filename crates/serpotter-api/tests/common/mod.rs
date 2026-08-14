// Shared integration-test fixture: each test binary uses a different subset
// of helpers, so individual unused-helper warnings are expected per target.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use http_body_util::BodyExt;
use serde_json::Value;
use serpotter_api::AppState;
use serpotter_db::connect_and_migrate;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::{ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient};

/// Fixed API token used across integration suites (valid length/shape).
pub const TEST_TOKEN: &str = "tok-validtokenfortest0000000000000000";

/// Admin secret wired into [`state_with`].
pub const TEST_ADMIN_SECRET: &str = "test-admin-secret";

/// Streamable HTTP clients must Accept both JSON and SSE (rmcp enforcement).
pub const MCP_ACCEPT: &str = "application/json, text/event-stream";

/// Full initialize params required by MCP (empty `{}` is rejected).
pub const MCP_INIT_BODY: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"serpotter-test","version":"0.1.0"}}}"#;

pub async fn test_db() -> serpotter_db::Db {
    connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate")
}

/// App state with providers pointed at `127.0.0.1:9` (connection refused, no network).
pub fn state_with(db: serpotter_db::Db) -> AppState {
    state_with_key_pool(
        db,
        /* max_inflight */ 3,
        Duration::from_secs(30),
        serpotter_db::KEY_HOLD_TTL_SECS,
    )
}

/// Like [`state_with`] but with explicit KeyPool limits (KeyBusy / hold tests).
pub fn state_with_key_pool(
    db: serpotter_db::Db,
    max_inflight: i64,
    acquire_timeout: Duration,
    hold_ttl_secs: i64,
) -> AppState {
    state_with_key_pool_and_proxy(db, max_inflight, acquire_timeout, hold_ttl_secs, false)
}

/// App state with `REQUIRE_OUTBOUND_PROXY` behavior: an empty node pool fails
/// closed with NoHealthyNode 503 instead of dialing direct. Mirrors the
/// production `ProxyPool::with_options(db, true)` (F59 NoHealthyNode path).
pub fn state_with_require_proxy(db: serpotter_db::Db) -> AppState {
    state_with_key_pool_and_proxy(
        db,
        /* max_inflight */ 3,
        Duration::from_secs(30),
        serpotter_db::KEY_HOLD_TTL_SECS,
        true,
    )
}

fn state_with_key_pool_and_proxy(
    db: serpotter_db::Db,
    max_inflight: i64,
    acquire_timeout: Duration,
    hold_ttl_secs: i64,
    require_proxy: bool,
) -> AppState {
    AppState {
        keys: Arc::new(KeyPool::with_config(
            db.clone(),
            max_inflight,
            acquire_timeout,
            hold_ttl_secs,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )),
        outbound: Arc::new(ProxyPool::with_options(db.clone(), require_proxy)),
        providers: ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        ),
        events: serpotter_api::events::RequestEvents::new(db.clone()).0,
        db,
        admin_secret: Some(TEST_ADMIN_SECRET.into()),
    }
}

pub async fn body_bytes(res: axum::response::Response) -> bytes::Bytes {
    res.into_body().collect().await.unwrap().to_bytes()
}

/// Parse MCP response body: plain JSON or SSE `data:` frames.
pub async fn body_json(res: axum::response::Response) -> Value {
    let bytes = body_bytes(res).await;
    let text = String::from_utf8_lossy(&bytes);
    parse_mcp_json_body(&text)
}

pub fn parse_mcp_json_body(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return serde_json::from_str(trimmed)
            .unwrap_or_else(|e| panic!("expected JSON body, got parse error {e}: {trimmed}"));
    }
    // SSE: last non-empty `data:` line that looks like JSON-RPC
    let mut last: Option<Value> = None;
    for line in trimmed.lines() {
        let line = line.trim();
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            last = Some(v);
        }
    }
    last.unwrap_or_else(|| panic!("no JSON data: frame in MCP body: {trimmed}"))
}

/// Build an authenticated MCP POST with required Streamable HTTP headers.
pub fn mcp_request(body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .body(body.into())
        .unwrap()
}

pub use axum::body::Body;
pub use axum::http::{Request, StatusCode};
pub use serpotter_api::app;
pub use tower::ServiceExt;
