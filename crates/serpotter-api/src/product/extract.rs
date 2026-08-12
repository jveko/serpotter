//! POST /api/extract and POST /api/research — auth, log, map product errors.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_product::{ExecMeta, ExtractRequest, ResearchRequest};

use super::errors::{extract_problem, research_problem};
use super::{deadline_detail, run_with_deadline, AppJson, DeadlineOutcome};
use crate::log_request::{
    self, fields_from_meta, request_id_from_headers, research_dial_label, ApiTokenLogged,
};
use crate::AppState;

#[tracing::instrument(skip_all, name = "extract")]
pub async fn extract_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJson(body): AppJson<ExtractRequest>,
) -> impl IntoResponse {
    let started = Instant::now();

    // B26: batch requests (urls) may legitimately omit the single `url`.
    let has_batch = body.urls.as_deref().is_some_and(|u| !u.is_empty());
    if body.url.trim().is_empty() && !has_batch {
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

    // FU10: reject an unknown extract provider at the boundary (400) instead of
    // letting it surface as a 502 "unknown extract provider" from the chain.
    if let Some(detail) = serpotter_core::validate_choice(
        "provider",
        body.provider.as_deref(),
        serpotter_core::VALID_EXTRACT_PROVIDERS,
    )
    .err()
    {
        let fields = fields_from_meta(
            "/api/extract",
            400,
            Some("ValidationError"),
            Some(log_request::query_preview(body.url.trim())),
            request_id_from_headers(&headers),
            Some(token.name),
            None,
            &ExecMeta::default(),
        );
        log_request::spawn_log(&state, fields, started);
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
    }

    let preview = log_request::query_preview(body.url.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    let ctx = state.product_ctx();

    let timeout = ctx.request_timeout;

    // B26/B27: the dispatch seam routes batch (urls), question/highlights
    // (format), structured (prompt/schema/output_schema) and the plain scrape
    // chain — REST and MCP share it.
    let call = async move { serpotter_product::extract_dispatch(&ctx, body).await };

    // F10: the whole product call runs under the per-request deadline.
    match run_with_deadline(timeout, call).await {
        DeadlineOutcome::Completed(Ok(o)) => {
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
        DeadlineOutcome::Completed(Err(o)) => {
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
        DeadlineOutcome::Elapsed => {
            // Holds (key/node leases) are released by their Drop safety nets
            // when the product future is dropped; nothing extra to do.
            let fields = fields_from_meta(
                "/api/extract",
                504,
                Some("Timeout"),
                Some(preview),
                request_id,
                token_name,
                None,
                &ExecMeta::default(),
            );
            log_request::spawn_log(&state, fields, started);
            problem_response(
                StatusCode::GATEWAY_TIMEOUT,
                "RequestTimeout",
                deadline_detail(timeout),
            )
        }
    }
}

#[tracing::instrument(skip_all, name = "research")]
pub async fn research_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJson(body): AppJson<ResearchRequest>,
) -> impl IntoResponse {
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

    // B17/B31 closed sets at the boundary: unknown backends / citation formats
    // are client errors (400), never silent fallbacks to the serpotter loop.
    if let Some(detail) = serpotter_core::validate_choice(
        "research_backend",
        body.research_backend.as_deref(),
        &["serpotter", "tavily"],
    )
    .err()
    .or_else(|| {
        serpotter_core::validate_choice(
            "citation_format",
            body.citation_format.as_deref(),
            &["numbered", "mla", "apa", "chicago"],
        )
        .err()
    }) {
        let fields = fields_from_meta(
            "/api/research",
            400,
            Some("ValidationError"),
            Some(preview.clone()),
            request_id_from_headers(&headers),
            token_name.clone(),
            None,
            &ExecMeta::default(),
        );
        log_request::spawn_log(&state, fields, started);
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
    }

    // F10: the whole product call runs under the per-request deadline.
    match run_with_deadline(
        ctx.request_timeout,
        serpotter_product::research_inner(&ctx, body),
    )
    .await
    {
        DeadlineOutcome::Completed(Ok(o)) => {
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
        DeadlineOutcome::Completed(Err(o)) => {
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
        DeadlineOutcome::Elapsed => {
            // Holds (key/node leases) are released by their Drop safety nets
            // when the product future is dropped; nothing extra to do.
            let fields = fields_from_meta(
                "/api/research",
                504,
                Some("Timeout"),
                Some(preview),
                request_id,
                token_name,
                None,
                &ExecMeta::default(),
            );
            log_request::spawn_log(&state, fields, started);
            problem_response(
                StatusCode::GATEWAY_TIMEOUT,
                "RequestTimeout",
                deadline_detail(ctx.request_timeout),
            )
        }
    }
}
