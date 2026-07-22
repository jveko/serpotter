//! POST /api/search — auth, log, map product errors to problem details.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_core::SearchQuery;
use serpotter_product::SearchExecError;

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
                None,
                Some(preview),
                started,
            );
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(SearchExecError::NoHealthyKey(msg)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/search",
                503,
                None,
                Some("NoHealthyKey"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey", msg)
        }
        Err(SearchExecError::Provider(msg)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/search",
                502,
                None,
                Some("ProviderError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::BAD_GATEWAY, "ProviderError", msg)
        }
        Err(SearchExecError::Search(msg)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/search",
                502,
                None,
                Some("SearchError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::BAD_GATEWAY, "SearchError", msg)
        }
        Err(SearchExecError::Db(e)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/search",
                500,
                None,
                Some("DatabaseError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", e.to_string())
        }
    }
}
