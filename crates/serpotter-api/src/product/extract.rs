//! POST /api/extract and POST /api/research — auth, log, map product errors.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_product::{ExtractRequest, ResearchRequest};

use super::errors::{extract_problem, research_problem};
use crate::{require_api_token, AppState};

pub async fn extract_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExtractRequest>,
) -> impl IntoResponse {
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }
    if body.url.trim().is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_url");
    }

    let started = Instant::now();
    let preview = crate::log_request::query_preview(body.url.trim());
    let ctx = state.product_ctx();

    match serpotter_product::extract_url(&ctx, body.url.trim(), body.provider.as_deref()).await {
        Ok(o) => {
            let r = o.result;
            let _meta = o.meta; // Task 3: pass into spawn_log
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
                200,
                Some(r.provider_used.clone()),
                Some(r.provider_used.clone()),
                None,
                Some(preview),
                started,
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(o) => {
            let e = o.result;
            let _meta = o.meta;
            let (code, status, kind, detail) = extract_problem(e);
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
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

pub async fn research_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResearchRequest>,
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

    match serpotter_product::research_inner(&ctx, body).await {
        Ok(o) => {
            let r = o.result;
            let _meta = o.meta;
            let provider_used = r
                .evidence
                .as_ref()
                .and_then(|e| e.providers_consulted.as_ref())
                .and_then(|p| p.first())
                .cloned();
            crate::log_request::spawn_log(
                &state,
                "/api/research",
                200,
                provider_used.clone(),
                provider_used,
                None,
                Some(preview),
                started,
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(o) => {
            let _meta = o.meta;
            let (code, status, kind, detail) = research_problem(o.result);
            crate::log_request::spawn_log(
                &state,
                "/api/research",
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
