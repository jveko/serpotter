//! POST /api/extract and POST /api/research — auth, log, map product errors.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_product::{ExecMeta, ExtractRequest, ResearchRequest};

use super::errors::{extract_problem, research_problem};
use crate::log_request::{self, fields_from_meta, request_id_from_headers, research_dial_label};
use crate::{require_api_token, AppState};

#[tracing::instrument(skip_all, name = "extract")]
pub async fn extract_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExtractRequest>,
) -> impl IntoResponse {
    let token = match require_api_token(&state, &headers).await {
        Ok(row) => row,
        Err(r) => return r,
    };
    let started = Instant::now();

    if body.url.trim().is_empty() {
        let fields = fields_from_meta(
            "/api/extract",
            400,
            Some("ValidationError"),
            None,
            request_id_from_headers(&headers),
            Some(token.name),
            None,
            &ExecMeta::default(),
        );
        log_request::spawn_log(&state, fields, started);
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_url");
    }

    let preview = log_request::query_preview(body.url.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    let ctx = state.product_ctx();

    match serpotter_product::extract_url(&ctx, body.url.trim(), body.provider.as_deref()).await {
        Ok(o) => {
            let r = o.result;
            let meta = o.meta;
            let fields = fields_from_meta(
                "/api/extract",
                200,
                None,
                Some(preview),
                request_id,
                token_name,
                Some(r.provider_used.clone()),
                &meta,
            );
            log_request::spawn_log(&state, fields, started);
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(o) => {
            let e = o.result;
            let meta = o.meta;
            let (code, status, kind, detail) = extract_problem(e);
            let fields = fields_from_meta(
                "/api/extract",
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

#[tracing::instrument(skip_all, name = "research")]
pub async fn research_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResearchRequest>,
) -> impl IntoResponse {
    let token = match require_api_token(&state, &headers).await {
        Ok(row) => row,
        Err(r) => return r,
    };
    let started = Instant::now();

    if body.query.trim().is_empty() {
        let fields = fields_from_meta(
            "/api/research",
            400,
            Some("ValidationError"),
            None,
            request_id_from_headers(&headers),
            Some(token.name),
            None,
            &ExecMeta::default(),
        );
        log_request::spawn_log(&state, fields, started);
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_query");
    }

    let preview = log_request::query_preview(body.query.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    let ctx = state.product_ctx();

    match serpotter_product::research_inner(&ctx, body).await {
        Ok(o) => {
            let r = o.result;
            let meta = o.meta;
            // Dial label: strategy with verify→blend-verify; strategy column stays raw.
            let provider_used = research_dial_label(&meta);
            let fields = fields_from_meta(
                "/api/research",
                200,
                None,
                Some(preview),
                request_id,
                token_name,
                provider_used,
                &meta,
            );
            log_request::spawn_log(&state, fields, started);
            (StatusCode::OK, Json(r)).into_response()
        }
        Err(o) => {
            let meta = o.meta;
            let (code, status, kind, detail) = research_problem(o.result);
            let fields = fields_from_meta(
                "/api/research",
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
