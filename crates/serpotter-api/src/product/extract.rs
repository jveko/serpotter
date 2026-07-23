//! POST /api/extract and POST /api/research — auth, log, map product errors.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_product::{ExtractError, ExtractRequest, ResearchError, ResearchRequest, SearchExecError};

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
        Ok(r) => {
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
                200,
                Some(r.provider_used.clone()),
                None,
                Some(preview),
                started,
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(ExtractError::NoHealthyKey(m)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
                503,
                None,
                Some("NoHealthyKey"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey", m)
        }
        Err(ExtractError::InvalidUrl(m)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
                400,
                None,
                Some("ValidationError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::BAD_REQUEST, "ValidationError", m)
        }
        Err(ExtractError::Provider(m)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
                502,
                None,
                Some("ProviderError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::BAD_GATEWAY, "ProviderError", m)
        }
        Err(ExtractError::Db(e)) => {
            crate::log_request::spawn_log(
                &state,
                "/api/extract",
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
        Ok(r) => {
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
                provider_used,
                None,
                Some(preview),
                started,
            );
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(ResearchError::Search(SearchExecError::NoHealthyKey(m)))
        | Err(ResearchError::Extract(ExtractError::NoHealthyKey(m))) => {
            crate::log_request::spawn_log(
                &state,
                "/api/research",
                503,
                None,
                Some("NoHealthyKey"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey", m)
        }
        Err(ResearchError::Extract(ExtractError::InvalidUrl(m))) => {
            crate::log_request::spawn_log(
                &state,
                "/api/research",
                400,
                None,
                Some("ValidationError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::BAD_REQUEST, "ValidationError", m)
        }
        Err(ResearchError::Search(SearchExecError::Provider(m)))
        | Err(ResearchError::Search(SearchExecError::Search(m)))
        | Err(ResearchError::Extract(ExtractError::Provider(m))) => {
            crate::log_request::spawn_log(
                &state,
                "/api/research",
                502,
                None,
                Some("ProviderError"),
                Some(preview),
                started,
            );
            problem_response(StatusCode::BAD_GATEWAY, "ProviderError", m)
        }
        Err(ResearchError::Search(SearchExecError::Db(e)))
        | Err(ResearchError::Extract(ExtractError::Db(e))) => {
            crate::log_request::spawn_log(
                &state,
                "/api/research",
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
