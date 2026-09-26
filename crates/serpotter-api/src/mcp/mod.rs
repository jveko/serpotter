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

mod admission;
mod auth;
mod errors;
mod params;
mod progress;

use admission::{Admission, MCP_MAX_INFLIGHT_PER_TOKEN};
use axum::http::{HeaderName, HeaderValue};
use errors::tool_error_structured;
use tower_http::cors::{AllowHeaders, CorsLayer, ExposeHeaders};

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

use crate::product::errors::{extract_err_log, research_err_log, search_err_log};
use crate::product::{deadline_detail, install_meta_sink};
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
    // Admission bookkeeping (per-token in-flight cap + session↔token
    // bindings) is per-process state, so it is created here and shared by
    // the transport, the middleware and every tool handler.
    let admission = Arc::new(Admission::new(
        MCP_MAX_INFLIGHT_PER_TOKEN,
        Duration::from_secs(MCP_SESSION_TTL_SECS),
    ));
    let for_handlers = admission.clone();
    let mcp_service: StreamableHttpService<SerpotterMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || {
                Ok(SerpotterMcp::new(
                    product.clone(),
                    expected,
                    events.clone(),
                    admission.clone(),
                ))
            },
            Arc::new(session_manager),
            config,
        );

    let layer_state = auth::McpLayerState {
        app: state,
        admission: for_handlers,
    };
    let mcp_service = tower::ServiceBuilder::new()
        .layer(middleware::from_fn_with_state(
            layer_state,
            mcp_auth_middleware,
        ))
        .service(mcp_service);
    // CORS runs OUTSIDE auth so a preflight is answered from the allowlist
    // alone, without a token. Host allowlist behavior is untouched — that
    // still lives in rmcp's config above.
    tower::ServiceBuilder::new()
        .layer(cors_layer(allowed_cors_origins()))
        .service(mcp_service)
}

/// CORS layer driven by `MCP_ALLOWED_ORIGINS`.
///
/// - Some origins: an allowlist (`Access-Control-Allow-Origin` is emitted
///   ONLY for a listed origin), the four methods the transport serves, and the
///   response headers a browser client must be able to READ (the session id
///   above all).
/// - None: a bare layer. The preflight still answers 200 without a token —
///   it is not an MCP request and must never be a 401 — but no
///   `Access-Control-*` header is emitted (the default allow-origin is an
///   EMPTY list, so nothing matches), so a browser still cannot call `/mcp`.
///   Operators who want browser clients must set the allowlist; an
///   unset/empty value is "no browser support", never "allow any origin".
///   The layer is installed either way so the return type stays concrete.
///
/// **Origin matching is byte-exact**, and the configured entries are
/// normalized at parse time (see [`mcp_allowed_origins`]) so they line up
/// with what a browser actually sends and with what rmcp's own `Origin`
/// validation accepts. Two consequences worth knowing:
/// - `*` is REJECTED (dropped with a warning), never turned into allow-any.
///   tower-http's `AllowOrigin::list` panics on a wildcard entry, so passing
///   one through would turn a misconfiguration into a startup crash. This
///   deployment's posture is allowlist-only.
/// - Allowed request headers are ECHOED from `Access-Control-Request-Headers`
///   (`mirror_request`) rather than enumerated. `Mcp-Param-*` is a header
///   PREFIX family and `AllowHeaders::list` cannot express a prefix; a literal
///   `Mcp-Param-*` entry would never match in a browser, so enumerating it
///   would be worse than useless. The origin allowlist is the security
///   boundary here — a request that passes it may send whatever headers it
///   asks to, which is the same trust the transport already requires for the
///   token in `Authorization`.
///
/// Side effect of installing the layer at all: tower-http adds
/// `Vary: origin, access-control-request-method, access-control-request-headers`
/// to EVERY response, configured or not. That is correct (the response does
/// depend on those request headers) and harmless; it is called out so nobody
/// later reads "unset → no CORS headers" as "unset → no CORS-derived header
/// of any kind".
fn cors_layer(origins: Option<Vec<HeaderValue>>) -> CorsLayer {
    let Some(origins) = origins else {
        return CorsLayer::new();
    };
    CorsLayer::new()
        .allow_origin(origins)
        .allow_headers(AllowHeaders::mirror_request())
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .expose_headers(ExposeHeaders::list(mcp_cors_exposed_headers()))
}

/// Parse `MCP_ALLOWED_ORIGINS` into the exact byte strings a browser will
/// echo back in its `Origin` header. Pure, so every case is unit-testable
/// without touching process env.
///
/// The two halves of the browser story must agree, and neither normalizes the
/// INCOMING `Origin` (tower-http compares it with a plain `Vec::contains`):
///
/// 1. `MCP_ALLOWED_ORIGINS` is parsed by rmcp into (scheme, host, port) and
///    matched with **lowercased** scheme/host and an omitted port acting as a
///    wildcard (`origin_is_allowed`). So a configured `https://App.Example.com`
///    lets rmcp serve that browser.
/// 2. tower-http compares the raw header against our list. Left unnormalized,
///    the mixed-case entry above would serve the request and then WITHHOLD
///    `Access-Control-Allow-Origin` — the silent breakage this function
///    prevents.
///
/// So each entry is lowercased, and a DEFAULT port for its scheme is stripped:
/// browsers omit `:443` on https and `:80` on http from the `Origin` header,
/// so writing either explicitly would otherwise never match. Non-default
/// ports are kept verbatim — those are meaningful and must be written exactly
/// as the browser sends them.
///
/// Unset, empty, all-junk, and wildcard-only lists all yield `None` (no CORS
/// configuration). A `*` entry is dropped with a warning: `AllowOrigin::list`
/// panics on one, and this deployment never means "allow any origin".
fn mcp_allowed_origins(raw: Option<&str>) -> Option<Vec<HeaderValue>> {
    let mut saw_wildcard = false;
    let values: Vec<HeaderValue> = raw?
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter(|s| {
            if *s == "*" {
                saw_wildcard = true;
                false
            } else {
                true
            }
        })
        .filter_map(|s| normalize_origin_entry(s).and_then(|v| HeaderValue::from_str(&v).ok()))
        .collect();
    if saw_wildcard {
        tracing::warn!(
            "MCP_ALLOWED_ORIGINS contains '*': wildcard origins are not supported and no \
             CORS headers will be emitted for it; list the origins explicitly"
        );
    }
    (!values.is_empty()).then_some(values)
}

/// Lowercase the scheme + host of one configured origin and drop a default
/// port, so the entry matches the `Origin` header a browser serializes.
/// Returns `None` for anything that is not a usable origin.
fn normalize_origin_entry(raw: &str) -> Option<String> {
    let uri: axum::http::Uri = raw.parse().ok()?;
    let scheme = uri.scheme_str()?.to_ascii_lowercase();
    let authority = uri.authority()?;
    let host = authority.host().to_ascii_lowercase();
    // `authority()?` above already guarantees a host, but require it to be
    // non-empty: "https://" alone is not an origin.
    if host.is_empty() {
        return None;
    }
    Some(match (scheme.as_str(), authority.port_u16()) {
        ("http", Some(80)) | ("https", Some(443)) | (_, None) => format!("{scheme}://{host}"),
        (_, Some(port)) => format!("{scheme}://{host}:{port}"),
    })
}

/// The configured allowlist, read once at service-build time.
///
/// The same comma-separated origins also feed rmcp's own `Origin` validation
/// above, so one variable drives both halves of browser support: the
/// preflight/response headers (here) and the spec-MUST `Origin` check (rmcp).
fn allowed_cors_origins() -> Option<Vec<HeaderValue>> {
    mcp_allowed_origins(std::env::var("MCP_ALLOWED_ORIGINS").ok().as_deref())
}

/// Response headers a browser client must be able to READ. Without these the
/// CORS work is incomplete: `fetch` only surfaces CORS-safelisted response
/// headers (`Content-Type`, `Cache-Control`, …), so a client that completes
/// preflight and `initialize` could not see the `Mcp-Session-Id` it must echo
/// on every session-scoped call — nor the protocol/correlation headers the
/// stateless path returns.
fn mcp_cors_exposed_headers() -> Vec<HeaderName> {
    [
        MCP_SESSION_HEADER,
        "mcp-protocol-version",
        "last-event-id",
        "x-request-id",
    ]
    .into_iter()
    .filter_map(|h| HeaderName::from_bytes(h.as_bytes()).ok())
    .collect()
}

#[derive(Clone)]
struct SerpotterMcp {
    product: ProductCtx,
    expected_schema_version: i64,
    events: Arc<crate::events::RequestEvents>,
    admission: Arc<Admission>,
    tool_router: ToolRouter<Self>,
}

impl SerpotterMcp {
    fn new(
        product: ProductCtx,
        expected_schema_version: i64,
        events: Arc<crate::events::RequestEvents>,
        admission: Arc<Admission>,
    ) -> Self {
        Self {
            product,
            expected_schema_version,
            events,
            admission,
            tool_router: Self::tool_router(),
        }
    }
}

impl SerpotterMcp {
    /// Per-token admission for a long-running tool call: take one in-flight
    /// permit, or answer the retryable `KeyBusy` envelope.
    ///
    /// Called by each handler FIRST — before any log-context resolution, sink
    /// construction, or vendor work — so an over-cap caller costs exactly one
    /// semaphore try. In particular the refusal performs NO database read:
    /// both fields its request row needs (`token_name`, `requestId`) are
    /// already in `Parts` — the token row stashed by `mcp_auth_middleware`,
    /// and the `x-request-id` header. Resolving them through
    /// `resolve_mcp_log_ctx` would issue a `get_token_by_value` per refused
    /// call, queueing the abusive token behind the very work the cap protects.
    ///
    /// The permit is held by the handler for the whole call, so the slot is
    /// released exactly when the request finishes (or its future is dropped).
    ///
    /// `Ok(None)` means the request carries no authenticated token (the
    /// handler was reached without going through the auth layer, which the
    /// production stack cannot do); it is left unlimited rather than refused,
    /// so this stays a tool-level guard and not a second auth mechanism.
    ///
    /// Refusal answers an ordinary tool RESULT (`isError:true` with the
    /// envelope), not a JSON-RPC error: an over-cap call is a back-pressure
    /// outcome the agent should see and retry, in the same shape as every
    /// other tool failure.
    fn admit(
        &self,
        name: &'static str,
        started: Instant,
        parts: &Parts,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, CallToolResult> {
        let Some(row) = parts.extensions.get::<serpotter_db::TokenRow>() else {
            return Ok(None);
        };
        let Some(permit) = self.admission.try_acquire(row.id) else {
            let cap = self.admission.max_inflight();
            let request_id = crate::events::request_id_from_headers(&parts.headers);
            crate::events::emit(
                &self.events,
                crate::events::fields_from_meta(
                    name,
                    503,
                    Some("KeyBusy"),
                    None,
                    request_id.clone(),
                    Some(row.name.clone()),
                    None,
                    &ExecMeta::default(),
                ),
                started,
            );
            return Err(tool_error_structured(
                "KeyBusy",
                format!(
                    "too many concurrent tool calls for this token (limit {cap}); retry shortly"
                ),
                request_id,
            ));
        };
        Ok(Some(permit))
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
        // Admission FIRST, and before any log-context resolution: a refusal
        // must cost nothing but a semaphore try (see `admit`).
        let _permit = match self.admit("/mcp/search", started, &parts) {
            Ok(permit) => permit,
            Err(refused) => return Ok(refused),
        };
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
        description = "Scrape/extract a URL (default chain: Firecrawl then Tavily; `provider=exa` leads with Exa, then Firecrawl, then Tavily). Modes: single URL (default chain), batch `urls` (Tavily/Exa), `format=question` (Firecrawl) or `highlights` (Exa), `prompt`/`schema` structured extraction (Firecrawl), `format=markdown|text` (Tavily output format).",
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
        // Admission FIRST, and before any log-context resolution: a refusal
        // must cost nothing but a semaphore try (see `admit`).
        let _permit = match self.admit("/mcp/extract_url", started, &parts) {
            Ok(permit) => permit,
            Err(refused) => return Ok(refused),
        };
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
        // Admission FIRST, and before any log-context resolution: a refusal
        // must cost nothing but a semaphore try (see `admit`).
        let _permit = match self.admit("/mcp/research", started, &parts) {
            Ok(permit) => permit,
            Err(refused) => return Ok(refused),
        };
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
    async fn health(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let (token_name, request_id) =
            crate::events::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        let emit = |status, kind: Option<&'static str>| {
            crate::events::emit(
                &self.events,
                crate::events::fields_from_meta(
                    "/mcp/health",
                    status,
                    kind,
                    None,
                    request_id.clone(),
                    token_name.clone(),
                    None,
                    &ExecMeta::default(),
                ),
                started,
            );
        };
        // A storage fault is a FAILURE, not a readiness answer. Collapsing it
        // to `not_ready` in an `isError:false` bespoke body made a hard DB
        // outage read to an agent as success — and, because this handler took
        // no `Extension<Parts>`, it emitted no event at all, so the one tool
        // an operator would use to detect the outage left no trace. The real
        // driver text stays server-side (same promise as the REST
        // `DatabaseError` problem); the client gets the standard envelope.
        let version = match self.product.db.schema_version().await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "mcp health: schema version lookup failed");
                emit(500, Some("DatabaseError"));
                return Ok(tool_error_structured(
                    "DatabaseError",
                    "internal storage error".to_string(),
                    request_id,
                ));
            }
        };
        // An outdated schema is a real (non-retryable) failure too — the
        // server cannot serve correct results until it is migrated.
        if version < self.expected_schema_version {
            emit(503, Some("NotReady"));
            return Ok(tool_error_structured(
                "NotReady",
                format!(
                    "database schema {version} is older than the expected {}",
                    self.expected_schema_version
                ),
                request_id,
            ));
        }
        emit(200, None);
        let body = serde_json::json!({
            "status": "ready",
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
            let preview = Some(crate::events::query_preview(r.url.trim()));
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

/// Outcome of the product-future vs. cancel-vs-deadline race.
#[derive(Debug)]
enum Race<T> {
    /// The product future completed (Ok or Err) — the only arm that reaches
    /// the sink flush + terminal result.
    Done(T),
    /// The client disconnected (rmcp cancelled the request).
    Cancelled,
    /// The overall request deadline elapsed.
    Deadline,
}

/// Race the product future against the client's cancellation and the request
/// deadline.
///
/// `biased;` with the product future FIRST is load-bearing: `select!` polls
/// branches in RANDOM order otherwise, so when the vendor call and the
/// deadline are both ready in the same tick it could discard the real (and
/// already PAID for) `Ok` and answer a `Timeout` envelope — recording no
/// usage, no cost, and a 504 for work that succeeded. `biased` makes the poll
/// order explicit and matches the REST path, where `tokio::time::timeout`
/// polls the inner future before the timer. Reached deterministically by a
/// ready future racing an already-elapsed deadline.
async fn race_deadline<T, C: Future>(
    call: impl Future<Output = T>,
    cancel: C,
    request_timeout: Duration,
) -> Race<T> {
    tokio::select! {
        biased;
        r = call => Race::Done(r),
        _ = cancel => Race::Cancelled,
        _ = tokio::time::sleep(request_timeout) => Race::Deadline,
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
    // The product ctx with this request's progress sink wired in, plus the
    // F10 attribution sink. Both MUST be installed before the product future
    // is built (it borrows `base`), so the install goes through the shared
    // `install_meta_sink` helper rather than a hand-rolled `Arc::new` — the
    // ordering contract lives in exactly one place.
    let mut base = base;
    install_meta_sink(&mut base);
    let product = ProductCtx {
        progress: Some(sink.clone()),
        ..base
    };
    // Read through the ctx the call actually runs on, so the cancel and
    // timeout arms below attribute the dropped future's real vendor.
    let meta_sink = product
        .meta_sink
        .clone()
        .expect("install_meta_sink always sets the sink");
    // The race itself lives in `race_deadline` (one owner for the poll order
    // that decides whether a paid-for result survives the tick); the three
    // outcomes are mapped to rows/envelopes here.
    let race = race_deadline(call(product, req), cancel, request_timeout).await;
    if !matches!(race, Race::Done(_)) {
        // EVERY early exit flushes first: the module's invariant is that a
        // queued progress frame reaches the transport before the terminal
        // result, on the cancel and deadline paths as much as on success.
        // `flush` is idempotent, so this cannot double-drain.
        sink.flush().await;
        let (status, kind, message) = match race {
            Race::Cancelled => (499, "Cancelled", "request cancelled by client".to_string()),
            // F10: overall request deadline elapsed — key/node holds are
            // released by their Drop safety nets when the future was dropped.
            _ => (504, "Timeout", deadline_detail(request_timeout)),
        };
        let fields = crate::events::fields_from_meta(
            name,
            status,
            Some(kind),
            preview.clone(),
            request_id.clone(),
            token_name,
            None,
            &meta_sink.last().unwrap_or_default(),
        );
        crate::events::emit(events, fields, started);
        return Ok(tool_error_structured(kind, message, request_id));
    }
    let Race::Done(outcome) = race else {
        unreachable!("the non-Done cases returned above")
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
mod cors_tests {
    use super::*;

    /// The origins a parse produced, as plain strings.
    fn parsed(raw: &str) -> Vec<String> {
        mcp_allowed_origins(Some(raw))
            .expect("a real list configures CORS")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect()
    }

    /// Unset / empty / junk must all mean "no CORS configuration" — never
    /// "allow anything", and never a partial allowlist that looks configured.
    #[test]
    fn allowed_origins_unset_empty_and_junk_configure_nothing() {
        assert!(mcp_allowed_origins(None).is_none(), "unset");
        assert!(mcp_allowed_origins(Some("")).is_none(), "empty");
        assert!(mcp_allowed_origins(Some("   ")).is_none(), "whitespace");
        assert!(
            mcp_allowed_origins(Some(",,,")).is_none(),
            "only separators"
        );
        // Not origins at all: no scheme, no host.
        assert!(mcp_allowed_origins(Some("nonsense")).is_none(), "junk only");
        assert!(mcp_allowed_origins(Some("https://")).is_none(), "no host");
    }

    /// `*` is dropped with a warning, never mapped to allow-any.
    ///
    /// This is a crash guard as much as a policy one: tower-http's
    /// `AllowOrigin::list` PANICS on a wildcard entry, so a `*` that reached
    /// `cors_layer` would take the process down at router build. Building the
    /// layer here proves the filtered value is safe to pass.
    #[test]
    fn wildcard_is_dropped_and_never_panics_the_layer() {
        assert!(
            mcp_allowed_origins(Some("*")).is_none(),
            "a wildcard-only list must configure nothing, not allow-any"
        );
        // Mixed with a real origin: the wildcard is dropped, the real one
        // survives, and constructing the layer is panic-free.
        let mixed = parsed("*,https://app.example.com");
        assert_eq!(mixed, vec!["https://app.example.com"]);
        let _layer = cors_layer(mcp_allowed_origins(Some("*,https://app.example.com")));
    }

    /// Entries are normalized to what a browser actually sends, and to what
    /// rmcp's own `Origin` validation accepts (lowercased scheme/host, default
    /// port acting as a wildcard). Without this, rmcp would serve a
    /// mixed-case origin while tower-http withheld `ACAO`.
    #[test]
    fn entries_are_normalized_to_the_browser_serialized_form() {
        assert_eq!(
            parsed("https://App.Example.COM"),
            vec!["https://app.example.com"],
            "scheme + host lowercase"
        );
        assert_eq!(
            parsed("https://app.example.com:443"),
            vec!["https://app.example.com"],
            "https default port stripped (browsers omit it)"
        );
        assert_eq!(
            parsed("http://app.example.com:80"),
            vec!["http://app.example.com"],
            "http default port stripped"
        );
        assert_eq!(
            parsed("http://localhost:5173"),
            vec!["http://localhost:5173"],
            "a non-default port is meaningful and kept"
        );
        assert_eq!(
            parsed(" https://a.example , HTTP://B.Example:8080 "),
            vec!["https://a.example", "http://b.example:8080"],
            "entries are trimmed and normalized independently"
        );
    }

    /// A browser can only READ a response header the layer exposes. The
    /// session id is the one it must echo on every session-scoped call, so
    /// omitting it from the exposed set would leave the browser flow this
    /// work exists for unusable.
    #[test]
    fn exposed_headers_include_the_session_and_protocol_ids() {
        let names: Vec<String> = mcp_cors_exposed_headers()
            .iter()
            .map(|h| h.as_str().to_string())
            .collect();
        assert!(
            names.contains(&MCP_SESSION_HEADER.to_string()),
            "Mcp-Session-Id must be readable by the client: {names:?}"
        );
        assert!(
            names.contains(&"mcp-protocol-version".to_string()),
            "the stateless path's protocol header must be readable: {names:?}"
        );
    }
}

#[cfg(test)]
mod race_tests {
    use super::*;

    /// A cancellation future that never fires (`async {}` would be ready on
    /// its first poll, which is a *cancelled* client, not an idle one).
    fn pending_cancel() -> impl Future<Output = ()> {
        std::future::pending()
    }

    /// The regression this pins: with the product future polled SECOND (or
    /// with a random poll order) an already-completed, already-elapsed-timer
    /// race could resolve to `Deadline` and throw the real result away. The
    /// product future must win.
    #[tokio::test]
    async fn completed_future_wins_a_same_tick_deadline() {
        // Zero timeout: the sleep is ready on its FIRST poll, so this is the
        // exact "both ready in one tick" situation with no timing luck.
        let out = race_deadline(async { 42u32 }, pending_cancel(), Duration::ZERO).await;
        assert!(
            matches!(out, Race::Done(42)),
            "a completed future must beat an already-elapsed deadline, got {out:?}"
        );
    }

    /// The deadline still wins when the product future is genuinely pending.
    #[tokio::test]
    async fn pending_future_still_times_out() {
        let out = race_deadline(
            std::future::pending::<u32>(),
            pending_cancel(),
            Duration::ZERO,
        )
        .await;
        assert!(
            matches!(out, Race::Deadline),
            "a never-completing future must time out, got {out:?}"
        );
    }

    /// A cancelled client wins over a ready deadline when the product future
    /// is still pending — the order between the two abort arms is cancel
    /// first, and neither is reachable once the product future completes.
    #[tokio::test]
    async fn cancel_wins_when_product_is_pending() {
        let out = race_deadline(std::future::pending::<u32>(), async {}, Duration::ZERO).await;
        assert!(
            matches!(out, Race::Cancelled),
            "a ready cancel must beat a ready deadline for a pending call, got {out:?}"
        );
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

    // Type-invalid args, NOT a missing `url`: batch extract legitimately
    // carries no `url` (T-mcp2), so `{}` now deserializes and is refused by
    // validation instead — a different row with a different message.
    #[test]
    fn prepare_extract_invalid_args_carries_no_preview() {
        let (message, preview) =
            prepare_extract(args(serde_json::json!({ "url": 42 }))).unwrap_err();
        assert!(message.starts_with("invalid args:"), "{message}");
        assert_eq!(preview, None, "args-parse row logs no preview");
    }

    #[test]
    fn prepare_extract_batch_needs_no_url() {
        let (r, preview) = prepare_extract(args(serde_json::json!({
            "urls": ["https://a.example", "https://b.example"],
        })))
        .expect("batch extract must not require `url`");
        assert_eq!(r.urls.unwrap().len(), 2);
        assert_eq!(
            preview.as_deref(),
            Some(""),
            "batch row logs an empty preview"
        );
    }

    #[test]
    fn prepare_extract_no_url_and_no_batch_is_a_validation_error() {
        let (detail, preview) = prepare_extract(args(serde_json::json!({}))).unwrap_err();
        assert!(detail.contains("missing url"), "{detail}");
        assert_eq!(preview, None, "extract conversion row logs no preview");
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
