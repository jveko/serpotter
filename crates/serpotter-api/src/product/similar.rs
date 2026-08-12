//! POST /api/similar — Exa findSimilar (B24), tok- auth, deadline, log.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serpotter_auth::problem_response;
use serpotter_product::ExecMeta;

use super::errors::{extract_err_log, extract_problem};
use super::{deadline_detail, run_with_deadline, AppJson, DeadlineOutcome};
use crate::log_request::{self, fields_from_meta, request_id_from_headers, ApiTokenLogged};
use crate::AppState;

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SimilarBody {
    pub url: String,
    #[serde(default)]
    pub max_results: Option<u32>,
}

/// POST /api/similar — pages similar to a URL (Exa findSimilar, title+url).
#[tracing::instrument(skip_all, name = "similar")]
pub async fn similar(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJson(body): AppJson<SimilarBody>,
) -> impl IntoResponse {
    let started = Instant::now();

    if body.url.trim().is_empty() {
        let fields = fields_from_meta(
            "/api/similar",
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

    match run_with_deadline(
        ctx.request_timeout,
        serpotter_product::find_similar(&ctx, body.url.trim(), body.max_results),
    )
    .await
    {
        DeadlineOutcome::Completed(Ok(o)) => {
            let r = o.result;
            let meta = o.meta;
            let fields = fields_from_meta(
                "/api/similar",
                200,
                None,
                Some(preview),
                request_id,
                token_name,
                Some("exa".into()),
                &meta,
            );
            log_request::spawn_log(&state, fields, started);
            (StatusCode::OK, Json(r)).into_response()
        }
        DeadlineOutcome::Completed(Err(o)) => {
            let e = o.result;
            let meta = o.meta;
            let (status, kind) = extract_err_log(&e);
            let fields = fields_from_meta(
                "/api/similar",
                status,
                Some(kind),
                Some(preview),
                request_id,
                token_name,
                None,
                &meta,
            );
            log_request::spawn_log(&state, fields, started);
            let (code, _, _, detail) = extract_problem(e);
            problem_response(code, kind, detail)
        }
        DeadlineOutcome::Elapsed => {
            let fields = fields_from_meta(
                "/api/similar",
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
