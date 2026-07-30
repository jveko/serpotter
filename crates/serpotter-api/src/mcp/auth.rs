use axum::body::Body;
use axum::extract::State;
use axum::http::Request;
use axum::middleware::Next;
use axum::response::Response;

use crate::{require_api_token, AppState};

pub async fn mcp_auth_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let row = match require_api_token(&state, request.headers()).await {
        Ok(row) => row,
        Err(r) => return r,
    };
    // Tools read TokenRow via rmcp Extension<Parts> (Parts.extensions).
    request.extensions_mut().insert(row);
    next.run(request).await
}
