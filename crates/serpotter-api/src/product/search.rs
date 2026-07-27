//! POST /api/search — auth, log, map product errors to problem details.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_core::SearchQuery;

use super::errors::search_problem;
use crate::{require_api_token, AppState};

pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SearchQuery>,
) -> impl IntoResponse {
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }

    if body.query.trim().is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_query");
    }

    let started = Instant::now();
    let preview = crate::log_request::query_preview(body.query.trim());
    let ctx = state.product_ctx();

    match serpotter_product::search_inner(&ctx, body).await {
        Ok(resp) => {
            crate::log_request::spawn_log(
                &state,
                "/api/search",
                200,
                Some(resp.provider_used.clone()),
                Some(resp.provider_used.clone()),
                None,
                Some(preview),
                started,
            );
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            let (code, status, kind, detail) = search_problem(e);
            crate::log_request::spawn_log(
                &state,
                "/api/search",
                status,
                None,
                None,
                Some(kind),
                Some(preview),
                started,
            );
            problem_response(code, kind, detail)
        }
    }
}
