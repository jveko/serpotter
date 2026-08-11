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
use crate::log_request::{self, fields_from_meta, request_id_from_headers, ApiTokenLogged};
use crate::AppState;

/// FU10: REST must reject routing knobs outside the advertised closed sets
/// exactly like the MCP boundary does — resolve_strategy/resolve_intent
/// silently coerce unknown values (strategy→fast, mode→no-op, intent→
/// pass-through), which would mislead REST clients.
fn validate_search_query(body: &SearchQuery) -> Option<String> {
    use serpotter_core::{
        validate_choice, validate_sources, VALID_INTENTS, VALID_MODES, VALID_PROVIDERS,
        VALID_SEARCH_DEPTHS, VALID_STRATEGIES,
    };
    validate_choice("mode", body.mode.as_deref(), VALID_MODES)
        .err()
        .or_else(|| validate_choice("intent", body.intent.as_deref(), VALID_INTENTS).err())
        .or_else(|| validate_choice("strategy", body.strategy.as_deref(), VALID_STRATEGIES).err())
        .or_else(|| validate_choice("provider", body.provider.as_deref(), VALID_PROVIDERS).err())
        .or_else(|| {
            validate_choice(
                "search_depth",
                body.search_depth.as_deref(),
                VALID_SEARCH_DEPTHS,
            )
            .err()
        })
        // B11: sources are a closed set on REST too — unknown sources are
        // client errors, never silent no-ops.
        .or_else(|| {
            let sources = body
                .sources
                .as_ref()
                .map(|s| s.as_list())
                .unwrap_or_default();
            validate_sources("sources", &sources).err()
        })
}

#[tracing::instrument(skip_all, name = "search")]
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
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

    if let Some(detail) = validate_search_query(&body) {
        let fields = fields_from_meta(
            "/api/search",
            400,
            Some("ValidationError"),
            Some(log_request::query_preview(body.query.trim())),
            request_id_from_headers(&headers),
            Some(token.name),
            None,
            &ExecMeta::default(),
        );
        log_request::spawn_log(&state, fields, started);
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
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
