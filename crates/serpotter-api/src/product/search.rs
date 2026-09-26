//! POST /api/search — auth, log, map product errors to problem details.

use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serpotter_auth::problem_response_ext;
use serpotter_core::SearchQuery;
use serpotter_product::ExecMeta;

use super::errors::{kind_retryable, search_problem};
use super::{
    deadline_detail, install_meta_sink, run_with_deadline, AppJsonLogged, DeadlineOutcome,
};
use crate::events::{self, fields_from_meta, request_id_from_headers, ApiTokenLogged};
use crate::AppState;

/// FU10: REST must reject routing knobs outside the advertised closed sets
/// exactly like the MCP boundary does — resolve_strategy/resolve_intent
/// silently coerce unknown values (strategy→fast, mode→no-op, intent→
/// pass-through), which would mislead REST clients.
///
/// The matchers are the lenient `normalize_*` ones: `" Tavily "` or
/// `ultra_fast` unambiguously name one advertised member, so REST now
/// accepts them just like MCP does. The canonical form they *return* is
/// deliberately DISCARDED here: this boundary only decides accept/reject,
/// and `SearchQuery::canonicalize()` at the product entry is the single
/// rewrite owner for routing, the providers and the B1 cache key. Writing
/// canonical values back at the handler would create a second write path
/// that can drift from that one — do not "optimize" this into one.
///
/// `time_range` joins the closed sets: previously ANY string was forwarded
/// to the vendors unvalidated, so a junk value is now the wave's ONE
/// deliberate new refusal (a 400 replacing a silent junk pass-through).
fn validate_search_query(body: &SearchQuery) -> Option<String> {
    use serpotter_core::{
        normalize_choice, normalize_search_depth, normalize_sources, normalize_time_range,
        VALID_INTENTS, VALID_MODES, VALID_PROVIDERS, VALID_STRATEGIES,
    };
    normalize_choice("mode", body.mode.as_deref(), VALID_MODES)
        .err()
        .or_else(|| normalize_choice("intent", body.intent.as_deref(), VALID_INTENTS).err())
        .or_else(|| normalize_choice("strategy", body.strategy.as_deref(), VALID_STRATEGIES).err())
        .or_else(|| normalize_choice("provider", body.provider.as_deref(), VALID_PROVIDERS).err())
        .or_else(|| {
            // Tavily depths + Exa deep modes (B20/B29) share the knob.
            normalize_search_depth("search_depth", body.search_depth.as_deref()).err()
        })
        .or_else(|| normalize_time_range("time_range", body.time_range.as_deref()).err())
        // B11: sources are a closed set on REST too — unknown sources are
        // client errors, never silent no-ops.
        .or_else(|| {
            let sources = body
                .sources
                .as_ref()
                .map(|s| s.as_list())
                .unwrap_or_default();
            normalize_sources("sources", &sources).err()
        })
}

#[tracing::instrument(skip_all, name = "search")]
pub async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiTokenLogged(token): ApiTokenLogged,
    AppJsonLogged(body): AppJsonLogged<SearchQuery>,
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

    if let Some(detail) = validate_search_query(&body) {
        let fields = fields_from_meta(
            "/api/search",
            400,
            Some("ValidationError"),
            Some(events::query_preview(body.query.trim())),
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

    let preview = events::query_preview(body.query.trim());
    let request_id = request_id_from_headers(&headers);
    let token_name = Some(token.name);
    // F10 attribution: the sink must exist BEFORE the product future is built
    // (it borrows `ctx`), so an elapsing deadline can report the real vendor.
    let mut ctx = state.product_ctx();
    install_meta_sink(&mut ctx);

    // F10: the whole product call runs under the per-request deadline.
    match run_with_deadline(
        ctx.request_timeout,
        &ctx,
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
            events::emit(&state.events, fields, started);
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
                "/api/search",
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

#[cfg(test)]
mod tests {
    use super::validate_search_query;
    use serpotter_core::SearchQuery;

    /// Deserialize the way the REST handler does (camelCase wire shape), so
    /// these tests pin exactly what `/api/search` accepts, not a Rust-side
    /// approximation.
    fn body(json: serde_json::Value) -> SearchQuery {
        serde_json::from_value(json).expect("REST-shaped body must deserialize")
    }

    #[test]
    fn rest_accepts_equivalent_spellings_of_valid_knobs() {
        // Every one of these previously 400'd: the value names exactly one
        // advertised member, only its spelling differed.
        let b = body(serde_json::json!({
            "query": "rust",
            "provider": " Tavily ",
            "searchDepth": "ultra_fast",
            "timeRange": "W",
            "mode": "WEB",
            "strategy": "balanced.",
        }));
        assert_eq!(
            validate_search_query(&b),
            None,
            "spelling variants must pass"
        );
    }

    #[test]
    fn rest_still_rejects_unknown_provider_naming_the_value() {
        // The closed set itself is NOT loosened, and the message keeps the
        // pre-wave shape ("{field}: {v:?} is not a supported value (valid: …)").
        let b = body(serde_json::json!({ "query": "rust", "provider": "banana" }));
        let detail = validate_search_query(&b).expect("banana must stay a 400");
        assert!(detail.starts_with("provider: "), "{detail}");
        assert!(detail.contains("\"banana\""), "{detail}");
        assert!(detail.contains("is not a supported value"), "{detail}");
    }

    #[test]
    fn rest_rejects_junk_time_range() {
        // The one deliberate NEW refusal of the canonicalization wave:
        // time_range was never validated before and junk was forwarded raw.
        let b = body(serde_json::json!({ "query": "rust", "timeRange": "nonsense" }));
        let detail = validate_search_query(&b).expect("junk time_range must 400");
        assert!(detail.contains("time_range"), "{detail}");
        assert!(detail.contains("\"nonsense\""), "{detail}");
        // …while the canonical members and the d/w/m/y aliases all pass.
        for ok in ["day", "week", "month", "year", "D", " W ", "M.", "y"] {
            let b = body(serde_json::json!({ "query": "rust", "timeRange": ok }));
            assert_eq!(validate_search_query(&b), None, "{ok:?} must pass");
        }
    }

    /// The silent-drop bug these aliases exist to kill: `ResearchRequest`
    /// already accepted both spellings, but `SearchQuery` was camelCase-only,
    /// so a snake_case body lost the field entirely — `time_range:"nonsense"`
    /// was neither applied nor rejected, and the client got an unfiltered
    /// success instead of the 400 it was owed.
    #[test]
    fn rest_accepts_snake_case_knobs_and_still_validates_them() {
        let b = body(serde_json::json!({ "query": "rust", "time_range": "nonsense" }));
        let detail =
            validate_search_query(&b).expect("snake_case time_range must reach validation");
        assert!(detail.contains("\"nonsense\""), "{detail}");

        let b = body(serde_json::json!({
            "query": "rust",
            "max_results": 3,
            "include_domains": ["docs.rs"],
            "search_depth": "advanced",
            "include_content": true,
            "chunks_per_source": 2,
        }));
        assert_eq!(validate_search_query(&b), None, "valid snake_case body");
        assert_eq!(b.max_results, Some(3));
        assert_eq!(b.include_content, Some(true));
        assert_eq!(b.search_depth.as_deref(), Some("advanced"));
        assert_eq!(b.chunks_per_source, Some(2));
        assert_eq!(
            b.include_domains.as_ref().map(|v| v.as_list()),
            Some(vec!["docs.rs".to_string()])
        );
    }

    /// camelCase keeps working exactly as before — the aliases are additive,
    /// and the serialized (response/cache) form is still camelCase.
    #[test]
    fn rest_still_accepts_camel_case_after_aliasing() {
        let b = body(serde_json::json!({
            "query": "rust",
            "maxResults": 3,
            "includeDomains": ["docs.rs"],
            "searchDepth": "advanced",
            "timeRange": "week",
        }));
        assert_eq!(validate_search_query(&b), None);
        assert_eq!(b.max_results, Some(3));
        assert_eq!(b.time_range.as_deref(), Some("week"));
        assert_eq!(
            b.include_domains.as_ref().map(|v| v.as_list()),
            Some(vec!["docs.rs".to_string()])
        );
    }
}
