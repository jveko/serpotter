//! POST /api/extract and POST /api/research — auth, log, map product errors.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response_ext;
use serpotter_product::{ExecMeta, ExtractRequest, ResearchRequest};

use super::errors::{extract_problem, kind_retryable, research_problem};
use super::{
    deadline_detail, install_meta_sink, run_with_deadline, AppJsonLogged, DeadlineOutcome,
};
use crate::events::{
    self, fields_from_meta, request_id_from_headers, research_dial_label, ApiTokenLogged,
};
use crate::AppState;

#[tracing::instrument(skip_all, name = "extract")]
pub async fn extract_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJsonLogged(body): AppJsonLogged<ExtractRequest>,
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
        events::emit(&state.events, fields, started);
        return problem_response_ext(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "missing_url",
            &[(
                "retryable",
                serde_json::json!(kind_retryable("ValidationError")),
            )],
        );
    }

    // FU10: reject an unknown extract provider at the boundary (400) instead
    // of letting it surface as a 502 "unknown extract provider" from the
    // chain. Lenient on spelling (`normalize_choice`); the canonical form it
    // returns is DISCARDED here — `extract_dispatch` canonicalizes the body
    // before its provider comparisons and cache keys, so one rewrite owner
    // covers both surfaces.
    if let Some(detail) = serpotter_core::normalize_choice(
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
            Some(events::query_preview(body.url.trim())),
            request_id_from_headers(&headers),
            Some(token.name),
            None,
            &ExecMeta::default(),
        );
        events::emit(&state.events, fields, started);
        return problem_response_ext(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            detail,
            &[(
                "retryable",
                serde_json::json!(kind_retryable("ValidationError")),
            )],
        );
    }

    let preview = events::query_preview(body.url.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    // F10 attribution: the sink must exist BEFORE the dispatch future is built
    // (it borrows `ctx`), so an elapsing deadline can report the real vendor.
    let mut ctx = state.product_ctx();
    install_meta_sink(&mut ctx);

    let timeout = ctx.request_timeout;

    // B26/B27: the dispatch seam routes batch (urls), question/highlights
    // (format), structured (prompt/schema/output_schema) and the plain scrape
    // chain — REST and MCP share it.
    let call = serpotter_product::extract_dispatch(&ctx, body);

    // F10: the whole product call runs under the per-request deadline.
    match run_with_deadline(timeout, &ctx, call).await {
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
            events::emit(&state.events, fields, started);
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
            events::emit(&state.events, fields, started);
            problem_response_ext(
                code,
                kind,
                detail,
                &[("retryable", serde_json::json!(kind_retryable(kind)))],
            )
        }
        DeadlineOutcome::Elapsed(meta) => {
            // Holds (key/node leases) are released by their Drop safety nets
            // when the product future is dropped; nothing extra to do. `meta`
            // is the live snapshot the dropped future published, so the event
            // names the vendor/key/node the request was actually on.
            let fields = fields_from_meta(
                "/api/extract",
                504,
                Some("RequestTimeout"),
                Some(preview),
                request_id,
                token_name,
                None,
                &meta,
            );
            events::emit(&state.events, fields, started);
            problem_response_ext(
                StatusCode::GATEWAY_TIMEOUT,
                "RequestTimeout",
                deadline_detail(timeout),
                &[(
                    "retryable",
                    serde_json::json!(kind_retryable("RequestTimeout")),
                )],
            )
        }
    }
}

/// The research request-shape rules, in the order their refusals must
/// surface: closed sets, then `time_range`, then the deep-loop combination.
/// Order is part of the cross-surface contract — a body tripping two rules
/// must produce the same `detail` here and the same `message` over MCP — so
/// this is written to read side by side with `mcp::params::validate_research_params`,
/// which performs the same four checks against the same core members.
///
/// `time_range` is closed-set validated here too, through the SAME core
/// matcher the search boundary uses: research used to be the one surface that
/// forwarded any spelling raw, so `"W"`, `"week"` and `"WEEK "` were three
/// upstream bodies and three cache rows for one request, and a typo was
/// either ignored by the vendor or charged to our quota.
fn validate_research_body(body: &ResearchRequest) -> Result<(), String> {
    serpotter_core::normalize_choice(
        "research_backend",
        body.research_backend.as_deref(),
        serpotter_core::VALID_RESEARCH_BACKENDS,
    )?;
    serpotter_core::normalize_choice(
        "citation_format",
        body.citation_format.as_deref(),
        serpotter_core::VALID_CITATION_FORMATS,
    )?;
    serpotter_core::normalize_time_range("time_range", body.time_range.as_deref())?;
    serpotter_core::validate_deep_research_knobs(
        body.deep,
        body.research_backend.as_deref(),
        body.citation_format.as_deref(),
        body.social_max_results,
        body.include_content,
    )
}

#[tracing::instrument(skip_all, name = "research")]
pub async fn research_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJsonLogged(body): AppJsonLogged<ResearchRequest>,
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
        events::emit(&state.events, fields, started);
        return problem_response_ext(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "missing_query",
            &[(
                "retryable",
                serde_json::json!(kind_retryable("ValidationError")),
            )],
        );
    }

    let preview = events::query_preview(body.query.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    // F10 attribution: the sink must exist BEFORE the research future is built
    // (it borrows `ctx`), so an elapsing deadline can report the real vendor.
    let mut ctx = state.product_ctx();
    install_meta_sink(&mut ctx);

    // B17/B31 closed sets, `time_range`, and the deep-loop combination rule.
    // All four checks live in `validate_research_body` and read their member
    // sets from core, which the MCP boundary also calls — so one request
    // cannot be accepted on one surface and refused on the other.
    if let Some(detail) = validate_research_body(&body).err() {
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
        events::emit(&state.events, fields, started);
        return problem_response_ext(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            detail,
            &[(
                "retryable",
                serde_json::json!(kind_retryable("ValidationError")),
            )],
        );
    }

    // F10: the whole product call runs under the per-request deadline.
    match run_with_deadline(
        ctx.request_timeout,
        &ctx,
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
            events::emit(&state.events, fields, started);
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
            events::emit(&state.events, fields, started);
            problem_response_ext(
                code,
                kind,
                detail,
                &[("retryable", serde_json::json!(kind_retryable(kind)))],
            )
        }
        DeadlineOutcome::Elapsed(meta) => {
            // Holds (key/node leases) are released by their Drop safety nets
            // when the product future is dropped; nothing extra to do. `meta`
            // is the live snapshot the dropped future published, so the event
            // names the vendor/key/node the request was actually on.
            let fields = fields_from_meta(
                "/api/research",
                504,
                Some("RequestTimeout"),
                Some(preview),
                request_id,
                token_name,
                None,
                &meta,
            );
            events::emit(&state.events, fields, started);
            problem_response_ext(
                StatusCode::GATEWAY_TIMEOUT,
                "RequestTimeout",
                deadline_detail(ctx.request_timeout),
                &[(
                    "retryable",
                    serde_json::json!(kind_retryable("RequestTimeout")),
                )],
            )
        }
    }
}
