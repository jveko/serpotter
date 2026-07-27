use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::{require_api_token, AppState};

pub async fn mcp_auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Err(r) = require_api_token(&state, request.headers()).await {
        return r;
    }
    next.run(request).await
}
