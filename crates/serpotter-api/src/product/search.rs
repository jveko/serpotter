//! POST /api/search — auth, log, map product errors to problem details.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_core::SearchQuery;

use super::errors::search_problem;
use crate::log_request::{self, fields_from_meta, request_id_from_headers};
use crate::{require_api_token, AppState};

#[tracing::instrument(skip_all, name = "search")]
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SearchQuery>,
) -> impl IntoResponse {
    let token = match require_api_token(&state, &headers).await {
        Ok(row) => row,
        Err(r) => return r,
    };

    if body.query.trim().is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_query");
    }

    let started = Instant::now();
    let preview = log_request::query_preview(body.query.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    let ctx = state.product_ctx();

    match serpotter_product::search_inner(&ctx, body).await {
        Ok(o) => {
            let resp = o.result;
            let meta = o.meta;
            let fields = fields_from_meta(
                "/api/search",
                200,
                None,
                Some(preview),
                request_id,
                token_name,
                Some(resp.provider_used.clone()),
                &meta,
            );
            log_request::spawn_log(&state, fields, started);
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(o) => {
            let meta = o.meta;
            let (code, status, kind, detail) = search_problem(o.result);
            let fields = fields_from_meta(
                "/api/search",
                status,
                Some(kind),
                Some(preview),
                request_id,
                token_name,
                None,
                &meta,
            );
            log_request::spawn_log(&state, fields, started);
            problem_response(code, kind, detail)
        }
    }
}
