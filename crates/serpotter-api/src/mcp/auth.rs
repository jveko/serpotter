use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::events;
use crate::{require_api_token, AppState};

/// Event path label for the MCP transport. This layer runs INSIDE
/// `nest_service("/mcp", …)`, which strips the prefix from the URI it hands
/// over, so the mount point is the only stable label.
const MCP_EVENT_PATH: &str = "/mcp";

pub async fn mcp_auth_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let row = match require_api_token(&state, request.headers()).await {
        Ok(row) => row,
        Err(r) => {
            // F08 parity: REST's `ApiTokenLogged` extractor emits an event for
            // every rejected token; MCP used to answer 401 with no ring row, no
            // error-window bucket and no metric, so a client hammering /mcp
            // with a bad token was invisible to the operator. Same field
            // constructor as the REST path, so the two cannot drift.
            // `token_name` stays None: the token does not exist.
            //
            // The path label is the MOUNT POINT, not `request.uri().path()`:
            // this layer is installed by `nest_service("/mcp", …)`, which
            // strips the prefix, so the inner URI is `/` and would file every
            // MCP auth failure under the REST catch-all `/api`.
            events::emit(
                &state.events,
                events::auth_failure_fields_for(MCP_EVENT_PATH, request.headers()),
                Instant::now(),
            );
            return r;
        }
    };
    // Tools read TokenRow via rmcp Extension<Parts> (Parts.extensions).
    request.extensions_mut().insert(row);
    next.run(request).await
}
