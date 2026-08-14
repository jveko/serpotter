//! MCP Streamable HTTP via official `rmcp` SDK — **dual-era**.
//!
//! Protocol 2026-07-28 is served **statelessly** (SEP-2567 is unconditional):
//! sessions, `initialize`, GET SSE and DELETE are gone for those requests —
//! every JSON-RPC message is its own POST to `/mcp`, answered with a single
//! JSON object (or SSE when the handler emits progress first), and
//! `server/discover` advertises the supported versions.
//!
//! Older clients (2025-11-25 and earlier) keep the legacy session path:
//! `initialize` → `Mcp-Session-Id` → GET stream / DELETE, via
//! `LocalSessionManager`. `stateless_protocol_metadata_required` applies only
//! to requests routed statelessly, so legacy sessions are unaffected.
//!
//! Tool args accept snake_case (preferred) and camelCase aliases.
//! Auth is outer axum middleware (Bearer / x-api-key) — session ≠ authentication.
//!
//! The long-running tools (search/extract/research) race the product future
//! against rmcp's per-request `CancellationToken`: a client that disconnects
//! (closes the stream) cancels the in-flight work early and logs a
//! 499/Cancelled request_log row.

mod auth;
mod errors;
mod params;
mod progress;

use errors::tool_error_structured;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::Request;
use axum::middleware;
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{schema_for_output, Extension};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, CompleteRequestParams, CompleteResult, CompletionInfo, ContentBlock,
    JsonObject, RequestMetaObject,
};
use rmcp::service::{Peer, RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serpotter_core::{SearchQuery, SearchResponse};
use serpotter_db::EXPECTED_SCHEMA_VERSION;
use serpotter_product::{
    ExecMeta, ExtractRequest, ExtractResponse, ProductCtx, ProductOutcome, ResearchRequest,
    ResearchResponse,
};

use auth::mcp_auth_middleware;
use params::{
    extract_params_to_request, research_params_to_request, search_params_to_query, ExtractParams,
    ResearchParams, SearchParams,
};
use progress::{structured_ok, McpProgressSink};

use crate::product::deadline_detail;
use crate::product::errors::{extract_err_log, research_err_log, search_err_log};
use crate::AppState;

/// Advertised output schema for a result-bearing tool: rmcp's
/// [`schema_for_output`] (top-level title/description stripped, output
/// schemas not restricted to root `"type": "object"`). `Arc<JsonObject>`
/// is the exact expression type the `#[tool]` `output_schema` attr expects.
fn output_schema<T: rmcp::schemars::JsonSchema + std::any::Any>(
) -> std::sync::Arc<serde_json::Map<String, serde_json::Value>> {
    schema_for_output::<T>()
}

/// Advertised input schema for a tool: `schema_for_input::<Parameters<T>>()`
/// (rmcp's stripped inputSchema, cached by TypeId). F19 takes the raw
/// `JsonObject` args so type-invalid arguments reach the handler (and the
/// error envelope) instead of failing rmcp's typed extraction, but the
/// advertised schema is still the rich `T` schema via this explicit
/// `input_schema` tool attribute.
fn input_schema<T: rmcp::schemars::JsonSchema + std::any::Any>(
) -> std::sync::Arc<serde_json::Map<String, serde_json::Value>> {
    rmcp::handler::server::common::schema_for_input::<Parameters<T>>()
        .expect("valid tool input schema")
}

/// Canonical session header (HTTP case-insensitive).
pub const MCP_SESSION_HEADER: &str = "mcp-session-id";
/// Documented product TTL target for LocalSessionManager keep-alive (legacy
/// clients only; 2026-07-28 requests are stateless).
pub const MCP_SESSION_TTL_SECS: u64 = 3600;

/// Build Streamable HTTP MCP service + tok- auth layer (mount with `nest_service("/mcp", …)`).
pub fn service(
    state: AppState,
) -> impl tower::Service<
    Request<Body>,
    Response = Response,
    Error = std::convert::Infallible,
    Future = impl Future<Output = Result<Response, std::convert::Infallible>> + Send,
> + Clone {
    let product = state.product_ctx();
    let expected = EXPECTED_SCHEMA_VERSION;

    // Dual-era: 2026-07-28 is always stateless (SEP-2567); older clients keep
    // sessions via LocalSessionManager. `json_response(true)` prefers plain
    // JSON for stateless terminal responses; a client `_meta.progressToken`
    // arms the per-request McpProgressSink, whose notification frames make
    // rmcp fall back to SSE. `stateless_protocol_metadata_required(true)`
    // enforces per-request protocolVersion/_meta on the stateless path only —
    // legacy sessions are exempt by design.
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = Some(Duration::from_secs(MCP_SESSION_TTL_SECS));
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(true)
        .with_json_response(true)
        .with_stateless_protocol_metadata_required(true);
    // Host validation is DNS-rebinding protection. Default: rmcp loopback-only.
    // Set MCP_ALLOWED_HOSTS=host,host:port (comma-separated) for public deploys.
    // Set MCP_ALLOWED_HOSTS= to empty to disable (not recommended).
    if let Ok(hosts) = std::env::var("MCP_ALLOWED_HOSTS") {
        let list: Vec<String> = hosts
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if list.is_empty() {
            config = config.disable_allowed_hosts();
        } else {
            config = config.with_allowed_hosts(list);
        }
    }
    // Origin validation (spec MUST when the header is present): set
    // MCP_ALLOWED_ORIGINS=https://app.example.com,http://localhost:5173 for
    // browser-origin clients; unset keeps rmcp's default (disabled).
    if let Ok(origins) = std::env::var("MCP_ALLOWED_ORIGINS") {
        let list: Vec<String> = origins
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        if list.is_empty() {
            config = config.disable_allowed_origins();
        } else {
            config = config.with_allowed_origins(list);
        }
    }

    // Cloned (not moved): `state` is still consumed by the auth middleware
    // layer below, and RequestEvents shares the same ring/error-window Arcs.
    let events = Arc::new(state.events.clone());
    let mcp_service: StreamableHttpService<SerpotterMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(SerpotterMcp::new(product.clone(), expected, events.clone())),
            Arc::new(session_manager),
            config,
        );

    tower::ServiceBuilder::new()
        .layer(middleware::from_fn_with_state(state, mcp_auth_middleware))
        .service(mcp_service)
}

#[derive(Clone)]
struct SerpotterMcp {
    product: ProductCtx,
    expected_schema_version: i64,
    events: Arc<crate::events::RequestEvents>,
    tool_router: ToolRouter<Self>,
}

impl SerpotterMcp {
    fn new(
        product: ProductCtx,
        expected_schema_version: i64,
        events: Arc<crate::events::RequestEvents>,
    ) -> Self {
        Self {
            product,
            expected_schema_version,
            events,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl SerpotterMcp {
    #[tool(
        description = "Multi-provider web search (routing + key filters: domains, dates, X handles, strategy/provider)",
        annotations(title = "Search", open_world_hint = true, read_only_hint = true, idempotent_hint = true),
        input_schema = input_schema::<SearchParams>(),
        output_schema = output_schema::<SearchResponse>(),
    )]
    async fn search(
        &self,
        args: JsonObject,
        context: RequestContext<RoleServer>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let (token_name, request_id) =
            crate::events::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        // rmcp cancels this token when the client sends notifications/cancelled
        // for this request id; abort early instead of running to completion.
        let sink = Arc::new(McpProgressSink::new(context.peer.clone(), &context.meta));
        run_tool(
            &self.events,
            "/mcp/search",
            "search",
            self.product.clone(),
            request_id,
            token_name,
            started,
            self.product.request_timeout,
            context.ct.cancelled(),
            sink,
            move || prepare_search(args),
            |product, body| async move { serpotter_product::search_inner(&product, body).await },
            |_meta, resp: &SearchResponse| Some(resp.provider_used.clone()),
            search_err_log,
        )
        .await
    }

    #[tool(
        description = "Scrape/extract a URL (Firecrawl first, then Tavily fallback)",
        annotations(title = "Extract URL", open_world_hint = true, read_only_hint = true, idempotent_hint = true),
        input_schema = input_schema::<ExtractParams>(),
        output_schema = output_schema::<ExtractResponse>(),
    )]
    async fn extract_url(
        &self,
        args: JsonObject,
        context: RequestContext<RoleServer>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let (token_name, request_id) =
            crate::events::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        let sink = Arc::new(McpProgressSink::new(context.peer.clone(), &context.meta));
        run_tool(
            &self.events,
            "/mcp/extract_url",
            "extract",
            self.product.clone(),
            request_id,
            token_name,
            started,
            self.product.request_timeout,
            context.ct.cancelled(),
            sink,
            move || prepare_extract(args),
            |product, request| async move {
                serpotter_product::extract_dispatch(&product, request).await
            },
            |_meta, resp: &ExtractResponse| Some(resp.provider_used.clone()),
            extract_err_log,
        )
        .await
    }

    #[tool(
        description = "Deep research: search then scrape; response keys webResults, scrapedPages, optional socialResults; include_content for full page text. Live notifications/progress when the client sends _meta.progressToken.",
        annotations(title = "Research", open_world_hint = true, read_only_hint = true, idempotent_hint = true),
        input_schema = input_schema::<ResearchParams>(),
        output_schema = output_schema::<ResearchResponse>(),
    )]
    async fn research(
        &self,
        args: JsonObject,
        context: RequestContext<RoleServer>,
        meta: RequestMetaObject,
        peer: Peer<RoleServer>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let (token_name, request_id) =
            crate::events::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        // Build the sink from the explicit peer/meta params: rmcp's
        // FromContextPart for RequestMetaObject swaps the meta out of the
        // context (`mem::swap`), so `context.meta` is empty here.
        let sink = Arc::new(McpProgressSink::new(peer.clone(), &meta));
        run_tool(
            &self.events,
            "/mcp/research",
            "research",
            self.product.clone(),
            request_id,
            token_name,
            started,
            self.product.request_timeout,
            context.ct.cancelled(),
            sink,
            move || prepare_research(args),
            |product, body| async move { serpotter_product::research_inner(&product, body).await },
            |meta, _resp: &ResearchResponse| crate::events::research_dial_label(meta),
            research_err_log,
        )
        .await
    }

    #[tool(
        name = "health",
        description = "Readiness and schema version (schemaVersion vs expected)",
        annotations(title = "Health", read_only_hint = true, open_world_hint = false)
    )]
    async fn health(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let version = self.product.db.schema_version().await.ok();
        let ready = version
            .map(|v| v >= self.expected_schema_version)
            .unwrap_or(false);
        let body = serde_json::json!({
            "status": if ready { "ready" } else { "not_ready" },
            "schemaVersion": version,
            "expected": self.expected_schema_version,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            body.to_string(),
        )]))
    }
}

// --- run_tool: shared MCP tool ceremony ------------------------------------
//
// The long-running tools (search/extract_url/research) share one ceremony:
// prepare the request (F19 raw-args deserialize + validation), wire the
// McpProgressSink into a per-request ProductCtx, race the product call
// against rmcp's per-request CancellationToken and the request deadline,
// flush queued progress frames, then map the outcome to a request_log row +
// the {kind,message,requestId} envelope. `run_tool` owns that ceremony once;
// the handlers above are thin data: name/fail_label, prepare, call, ok_log,
// err_kind. Behavior (envelopes + log rows) is identical to the old inline
// handlers — the mcp_*.rs integration suites pin it.

/// Result of a tool's prepare step: on Ok the parsed request plus the log
/// preview; on Err the ValidationError envelope message plus the preview for
/// the 400 log row. The previews reproduce the old inline handlers exactly
/// (e.g. `search`/`research` log `None` for the empty-query row but
/// `Some(preview)` for the params-conversion row; `extract` logs `None` for
/// its conversion row because the preview is computed after conversion).
type PrepareOutcome<R> = Result<(R, Option<String>), (String, Option<String>)>;

/// `search`'s prepare: F19 raw-args deserialize → non-empty query → convert.
fn prepare_search(args: JsonObject) -> PrepareOutcome<SearchQuery> {
    let p: SearchParams = match serde_json::from_value(serde_json::Value::Object(args)) {
        Ok(p) => p,
        Err(e) => return Err((format!("invalid args: {e}"), None)),
    };
    if p.query.trim().is_empty() {
        return Err(("missing query".to_string(), None));
    }
    let preview = crate::events::query_preview(p.query.trim());
    match search_params_to_query(p) {
        Ok(q) => Ok((q, Some(preview.clone()))),
        Err(e) => Err((format!("invalid search params: {e}"), Some(preview))),
    }
}

/// `extract_url`'s prepare: F19 raw-args deserialize → convert. The preview
/// comes from the converted request's URL — computed after conversion exactly
/// like the old inline handler, whose conversion-failure row logs `None`.
fn prepare_extract(args: JsonObject) -> PrepareOutcome<ExtractRequest> {
    let p: ExtractParams = match serde_json::from_value(serde_json::Value::Object(args)) {
        Ok(p) => p,
        Err(e) => return Err((format!("invalid args: {e}"), None)),
    };
    match extract_params_to_request(p) {
        Ok(r) => {
            let preview = Some(crate::events::query_preview(&r.url));
            Ok((r, preview))
        }
        Err(detail) => Err((detail, None)),
    }
}

/// `research`'s prepare: F19 raw-args deserialize → non-empty query → convert.
fn prepare_research(args: JsonObject) -> PrepareOutcome<ResearchRequest> {
    let p: ResearchParams = match serde_json::from_value(serde_json::Value::Object(args)) {
        Ok(p) => p,
        Err(e) => return Err((format!("invalid args: {e}"), None)),
    };
    if p.query.trim().is_empty() {
        return Err(("missing query".to_string(), None));
    }
    let preview = crate::events::query_preview(p.query.trim());
    match research_params_to_request(p) {
        Ok(r) => Ok((r, Some(preview.clone()))),
        Err(detail) => Err((detail, Some(preview))),
    }
}

/// Run one tool call under the shared MCP ceremony (see the section comment).
///
/// Semantics are identical to the pre-refactor inline handlers:
/// - prepare failure → 400 ValidationError row + envelope (the message the
///   prepare returned: `invalid args: …`, `missing query`,
///   `invalid search params: …`, or the params-conversion detail).
/// - client cancel → 499 Cancelled + "request cancelled by client".
/// - request deadline → 504 Timeout + `deadline_detail(request_timeout)`.
/// - ok → 200 row with `ok_log(meta, resp)` provider_used + `structured_ok`.
/// - err → `err_kind(&e)` status/kind row + "{fail_label} failed: {e}".
/// - `sink.flush()` runs before the terminal result (queued progress frames
///   must reach the transport first so rmcp's response builder picks SSE).
#[allow(clippy::too_many_arguments)]
async fn run_tool<Req, Resp, E, P, C, Fut, OK, ER>(
    events: &crate::events::RequestEvents,
    name: &'static str,
    fail_label: &'static str,
    base: ProductCtx,
    request_id: Option<String>,
    token_name: Option<String>,
    started: Instant,
    request_timeout: Duration,
    cancel: impl Future<Output = ()>,
    sink: Arc<McpProgressSink>,
    prepare: P,
    call: C,
    ok_log: OK,
    err_kind: ER,
) -> Result<CallToolResult, rmcp::ErrorData>
where
    Resp: serde::Serialize,
    E: std::fmt::Display,
    P: FnOnce() -> PrepareOutcome<Req>,
    C: FnOnce(ProductCtx, Req) -> Fut,
    Fut: Future<Output = Result<ProductOutcome<Resp>, ProductOutcome<E>>>,
    OK: FnOnce(&ExecMeta, &Resp) -> Option<String>,
    ER: FnOnce(&E) -> (i64, &'static str),
{
    let (req, preview) = match prepare() {
        Ok(ok) => ok,
        Err((message, preview)) => {
            let fields = crate::events::fields_from_meta(
                name,
                400,
                Some("ValidationError"),
                preview,
                request_id.clone(),
                token_name,
                None,
                &ExecMeta::default(),
            );
            crate::events::emit(events, fields, started);
            return Ok(tool_error_structured(
                "ValidationError",
                message,
                request_id,
            ));
        }
    };
    // The product ctx with this request's progress sink wired in (same
    // `{ progress: Some(sink), .. }` construction the handlers used inline).
    let product = ProductCtx {
        progress: Some(sink.clone()),
        ..base
    };
    let outcome = tokio::select! {
        r = call(product, req) => r,
        _ = cancel => {
            // client disconnected — queued progress frames drain when the sink drops
            let fields = crate::events::fields_from_meta(
                name,
                499,
                Some("Cancelled"),
                preview.clone(),
                request_id.clone(),
                token_name,
                None,
                &ExecMeta::default(),
            );
            crate::events::emit(events, fields, started);
            return Ok(tool_error_structured(
                "Cancelled",
                "request cancelled by client".to_string(),
                request_id,
            ));
        }
        _ = tokio::time::sleep(request_timeout) => {
            // F10: overall request deadline elapsed — key/node holds are
            // released by their Drop safety nets when the future is dropped.
            let fields = crate::events::fields_from_meta(
                name,
                504,
                Some("Timeout"),
                preview.clone(),
                request_id.clone(),
                token_name,
                None,
                &ExecMeta::default(),
            );
            crate::events::emit(events, fields, started);
            return Ok(tool_error_structured(
                "Timeout",
                deadline_detail(request_timeout),
                request_id,
            ));
        }
    };
    // Deliver queued progress frames before the terminal result: rmcp's
    // stateless response builder picks SSE only when a notification arrives
    // through the transport before the response.
    sink.flush().await;
    match outcome {
        Ok(o) => {
            let resp = o.result;
            let exec_meta = o.meta;
            let provider_used = ok_log(&exec_meta, &resp);
            let fields = crate::events::fields_from_meta(
                name,
                200,
                None,
                preview,
                request_id.clone(),
                token_name,
                provider_used,
                &exec_meta,
            );
            crate::events::emit(events, fields, started);
            structured_ok(resp, request_id)
        }
        Err(o) => {
            let e = o.result;
            let exec_meta = o.meta;
            let (status, kind) = err_kind(&e);
            let fields = crate::events::fields_from_meta(
                name,
                status,
                Some(kind),
                preview,
                request_id.clone(),
                token_name,
                None,
                &exec_meta,
            );
            crate::events::emit(events, fields, started);
            Ok(tool_error_structured(
                kind,
                format!("{fail_label} failed: {e}"),
                request_id,
            ))
        }
    }
}

// rmcp-macros requires `version` to be a string literal, so the hard-coded
// value would drift from the crate. Omitting it makes rmcp emit
// `Implementation::new(name, env!("CARGO_PKG_VERSION"))` — serverInfo.version
// stays in sync with serpotter-api's crate version automatically.
#[tool_handler(
    router = self.tool_router,
    name = "serpotter",
    instructions = "Serpotter multi-provider search, extract, and research tools"
)]
impl ServerHandler for SerpotterMcp {
    /// B30: `completion/complete` — argument autocomplete for the routing
    /// knobs (strategy/mode/intent/provider/source/search_depth) using the
    /// same closed sets the MCP + REST boundaries validate with. rmcp's
    /// `Reference` models prompts/resources only, so clients target the tool
    /// by its prompt name ("search", "extract_url", "research", "health");
    /// anything else answers an empty completion.
    async fn complete(
        &self,
        request: CompleteRequestParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<CompleteResult, rmcp::ErrorData> {
        Ok(complete_args(&request))
    }
}

/// Values for one argument name (closed sets from `serpotter_core::validation`).
fn completions_for(argument: &str) -> &'static [&'static str] {
    match argument {
        "strategy" => serpotter_core::VALID_STRATEGIES,
        "mode" => serpotter_core::VALID_MODES,
        "intent" => serpotter_core::VALID_INTENTS,
        "provider" => serpotter_core::VALID_PROVIDERS,
        "source" | "sources" => serpotter_core::VALID_SOURCES,
        "search_depth" => serpotter_core::VALID_SEARCH_DEPTHS,
        _ => &[],
    }
}

/// Prefix-match completions for a `completion/complete` request (B30).
///
/// Only tool-prompt references and known argument names produce values; an
/// empty prefix returns the whole set, a non-matching prefix returns nothing.
fn complete_args(request: &CompleteRequestParams) -> CompleteResult {
    let values: Vec<String> = match request.r#ref.as_prompt_name() {
        Some("search" | "extract_url" | "research" | "health") => {
            let prefix = request.argument.value.as_str();
            completions_for(request.argument.name.as_str())
                .iter()
                .filter(|v| v.starts_with(prefix))
                .map(|s| s.to_string())
                .collect()
        }
        _ => Vec::new(),
    };
    CompleteResult::new(CompletionInfo::with_all_values(values).unwrap_or_default())
}

#[cfg(test)]
mod complete_tests {
    use super::*;
    use rmcp::model::{ArgumentInfo, Reference};

    fn req(argument: &str, value: &str) -> CompleteRequestParams {
        CompleteRequestParams::new(
            Reference::for_prompt("search"),
            ArgumentInfo::new(argument, value),
        )
    }

    #[test]
    fn strategy_prefix_completion_returns_balanced() {
        let out = complete_args(&req("strategy", "ba"));
        assert_eq!(out.completion.values, vec!["balanced".to_string()]);
    }

    #[test]
    fn empty_prefix_returns_whole_set() {
        let out = complete_args(&req("strategy", ""));
        assert_eq!(
            out.completion.values,
            serpotter_core::VALID_STRATEGIES
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn mode_intent_sources_and_depth_complete() {
        let cases = [
            ("mode", "so", vec!["social"]),
            ("intent", "tut", vec!["tutorial"]),
            ("source", "x", vec!["x"]),
            ("sources", "web", vec!["web"]),
            ("search_depth", "ad", vec!["advanced"]),
            ("provider", "ta", vec!["tavily"]),
        ];
        for (arg, prefix, want) in cases {
            let out = complete_args(&req(arg, prefix));
            let got: Vec<&str> = out.completion.values.iter().map(|s| s.as_str()).collect();
            assert_eq!(got, want, "{arg} prefix {prefix:?}");
        }
    }

    #[test]
    fn unknown_argument_or_no_match_answers_empty() {
        let out = complete_args(&req("bogus", "x"));
        assert!(out.completion.values.is_empty());
        let out = complete_args(&req("strategy", "zzz"));
        assert!(out.completion.values.is_empty());
    }

    #[test]
    fn non_tool_reference_answers_empty() {
        let req = CompleteRequestParams::new(
            Reference::for_prompt("other"),
            ArgumentInfo::new("strategy", "ba"),
        );
        let out = complete_args(&req);
        assert!(out.completion.values.is_empty());
    }
}

#[cfg(test)]
mod prepare_tests {
    use super::*;

    /// Build the raw `JsonObject` args a handler would receive.
    fn args(v: serde_json::Value) -> JsonObject {
        v.as_object().expect("test args must be an object").clone()
    }

    #[test]
    fn prepare_search_invalid_args_carries_no_preview() {
        // `{}` fails SearchParams deserialization (required `query`).
        let (message, preview) = prepare_search(args(serde_json::json!({}))).unwrap_err();
        assert!(message.starts_with("invalid args:"), "{message}");
        assert_eq!(preview, None, "args-parse row logs no preview");
    }

    #[test]
    fn prepare_search_empty_query_carries_no_preview() {
        let (message, preview) =
            prepare_search(args(serde_json::json!({ "query": "   " }))).unwrap_err();
        assert_eq!(message, "missing query");
        assert_eq!(preview, None, "empty-query row logs no preview");
    }

    #[test]
    fn prepare_search_ok_preview_is_trimmed_query() {
        let (q, preview) = prepare_search(args(serde_json::json!({ "query": "  hello world  " })))
            .expect("valid search");
        // The wire query keeps the raw string; only the log preview is trimmed.
        assert_eq!(q.query, "  hello world  ");
        assert_eq!(preview.as_deref(), Some("hello world"));
    }

    #[test]
    fn prepare_search_params_failure_keeps_preview() {
        let (message, preview) = prepare_search(args(serde_json::json!({
            "query": "hello",
            "strategy": "bogus",
        })))
        .unwrap_err();
        assert!(message.starts_with("invalid search params:"), "{message}");
        assert_eq!(
            preview.as_deref(),
            Some("hello"),
            "params row keeps preview"
        );
    }

    #[test]
    fn prepare_extract_ok_preview_is_url() {
        let (r, preview) = prepare_extract(args(
            serde_json::json!({ "url": "https://example.com/page" }),
        ))
        .expect("valid extract");
        assert_eq!(r.url, "https://example.com/page");
        assert_eq!(preview.as_deref(), Some("https://example.com/page"));
    }

    #[test]
    fn prepare_extract_invalid_args_carries_no_preview() {
        let (message, preview) = prepare_extract(args(serde_json::json!({}))).unwrap_err();
        assert!(message.starts_with("invalid args:"), "{message}");
        assert_eq!(preview, None, "args-parse row logs no preview");
    }

    #[test]
    fn prepare_extract_conversion_failure_carries_no_preview() {
        let (detail, preview) = prepare_extract(args(
            serde_json::json!({ "url": "https://x", "format": "bogus" }),
        ))
        .unwrap_err();
        assert!(detail.contains("format"), "{detail}");
        assert_eq!(preview, None, "extract conversion row logs no preview");
    }

    #[test]
    fn prepare_research_empty_query_carries_no_preview() {
        let (message, preview) =
            prepare_research(args(serde_json::json!({ "query": "" }))).unwrap_err();
        assert_eq!(message, "missing query");
        assert_eq!(preview, None, "empty-query row logs no preview");
    }

    #[test]
    fn prepare_research_ok_preview_is_trimmed_query() {
        let (r, preview) = prepare_research(args(serde_json::json!({ "query": "deep research" })))
            .expect("valid research");
        assert_eq!(r.query, "deep research");
        assert_eq!(preview.as_deref(), Some("deep research"));
    }

    #[test]
    fn prepare_research_conversion_failure_keeps_preview() {
        let (detail, preview) = prepare_research(args(serde_json::json!({
            "query": "deep",
            "citation_format": "bogus",
        })))
        .unwrap_err();
        assert!(detail.contains("citation_format"), "{detail}");
        assert_eq!(preview.as_deref(), Some("deep"), "params row keeps preview");
    }
}
