//! GET /mcp SSE keep-alives and DELETE /mcp session terminate (Streamable HTTP subset).

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream;

use crate::mcp::session_from_headers;
use crate::{require_api_token, AppState};

/// Long-lived SSE channel for an MCP session (heartbeats via KeepAlive).
/// Requires API token and a live `mcp-session-id` (client must initialize first).
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
    // Heartbeats via KeepAlive; pending stream never ends until client disconnects.
    Sse::new(stream::pending::<Result<Event, Infallible>>())
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response()
}

/// Terminate an MCP session. Idempotent: missing/unknown session still 204 when header present.
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
