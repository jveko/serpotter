//! GET /mcp SSE keep-alives and DELETE /mcp session terminate (Streamable HTTP subset).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::unfold;

use crate::mcp::session_from_headers;
use crate::{require_api_token, AppState};

/// Max SSE lifetime (~5 minutes at 15s interval).
const MCP_SSE_MAX_TICKS: u32 = 20;
const MCP_SSE_INTERVAL_SECS: u64 = 15;

/// Long-lived SSE channel for an MCP session.
/// Requires API token and a live `mcp-session-id`.
/// Ends when session is DELETE'd/expired, or after ~5 minutes.
/// Uses a 15s loop with `touch` (not KeepAlive+pending) so DELETE is observed.
pub async fn mcp_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }
    let Some(id) = session_from_headers(&headers).map(|s| s.to_string()) else {
        return (StatusCode::BAD_REQUEST, "mcp-session-id required").into_response();
    };
    if !state.mcp_sessions.touch(&id) {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }

    let store = state.mcp_sessions.clone();
    let stream = unfold((store, id, 0u32), |(store, id, tick)| async move {
        if tick >= MCP_SSE_MAX_TICKS {
            return None;
        }
        tokio::time::sleep(Duration::from_secs(MCP_SSE_INTERVAL_SECS)).await;
        // Re-touch: false if DELETE removed the session or TTL expired.
        if !store.touch(&id) {
            return None;
        }
        let ev = Ok::<Event, Infallible>(Event::default().comment("keepalive"));
        Some((ev, (store, id, tick + 1)))
    });

    Sse::new(stream).into_response()
}

/// Terminate an MCP session. Idempotent 204 when header present.
/// Open GET streams for this id end on the next 15s tick after remove.
pub async fn mcp_delete(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }
    let Some(id) = session_from_headers(&headers) else {
        return (StatusCode::BAD_REQUEST, "mcp-session-id required").into_response();
    };
    let _ = state.mcp_sessions.remove(id);
    StatusCode::NO_CONTENT.into_response()
}
