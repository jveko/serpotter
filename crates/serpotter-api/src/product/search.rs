//! POST /api/search — auth, log, map product errors to problem details.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response;
use serpotter_core::SearchQuery;
use serpotter_product::ExecMeta;

use super::errors::search_problem;
use super::{deadline_detail, run_with_deadline, AppJson, DeadlineOutcome};
use crate::log_request::{self, fields_from_meta, request_id_from_headers};
use crate::{ApiToken, AppState};

#[tracing::instrument(skip_all, name = "search")]
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiToken(token): ApiToken,
    AppJson(body): AppJson<SearchQuery>,
) -> impl IntoResponse {
    let started = Instant::now();

    if body.query.trim().is_empty() {
        let fields = fields_from_meta(
            "/api/search",
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

    // F10: the whole product call runs under the per-request deadline.
    match run_with_deadline(
        ctx.request_timeout,
        serpotter_product::search_inner(&ctx, body),
    )
    .await
    {
        DeadlineOutcome::Completed(Ok(o)) => {
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
        DeadlineOutcome::Completed(Err(o)) => {
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
        DeadlineOutcome::Elapsed => {
            // Holds (key/node leases) are released by their Drop safety nets
            // when the product future is dropped; nothing extra to do.
            let fields = fields_from_meta(
                "/api/search",
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
