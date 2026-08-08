//! MCP Streamable HTTP via official `rmcp` SDK.
//!
//! Tool args accept snake_case (preferred) and camelCase aliases.
//! Auth is outer axum middleware (Bearer / x-api-key) — session ≠ authentication.
//!
//! The long-running tools (search/extract/research) race the product future
//! against rmcp's per-request `CancellationToken`: a client `notifications/cancelled`
//! aborts the in-flight work early and logs a 499/Cancelled request_log row.
//! rmcp intentionally drops the JSON-RPC response for a cancelled request.

mod auth;
mod errors;
mod params;
mod progress;

use errors::tool_error;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::request::Parts;
use axum::http::Request;
use axum::middleware;
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Meta};
use rmcp::service::{Peer, RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{tool, tool_handler, tool_router, ServerHandler};
use serpotter_db::EXPECTED_SCHEMA_VERSION;
use serpotter_product::{ProductCtx, ResearchRequest};

use auth::mcp_auth_middleware;
use params::{
    mcp_list_to_vec_or_one, search_params_to_query, ExtractParams, ResearchParams, SearchParams,
};
use progress::{soft_progress, text_ok};

use crate::product::errors::{extract_err_log, research_err_log, search_err_log};
use crate::AppState;

/// Canonical session header (HTTP case-insensitive).
pub const MCP_SESSION_HEADER: &str = "mcp-session-id";
/// Documented product TTL target for LocalSessionManager keep-alive.
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

    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = Some(Duration::from_secs(MCP_SESSION_TTL_SECS));
    // Host validation is DNS-rebinding protection. Default: rmcp loopback-only.
    // Set MCP_ALLOWED_HOSTS=host,host:port (comma-separated) for public deploys.
    // Set MCP_ALLOWED_HOSTS= to empty to disable (not recommended).
    let mut config = StreamableHttpServerConfig::default().with_stateful_mode(true);
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

    let mcp_service: StreamableHttpService<SerpotterMcp, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(SerpotterMcp::new(product.clone(), expected)),
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
    tool_router: ToolRouter<Self>,
}

impl SerpotterMcp {
    fn new(product: ProductCtx, expected_schema_version: i64) -> Self {
        Self {
            product,
            expected_schema_version,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl SerpotterMcp {
    #[tool(
        description = "Multi-provider web search (routing + key filters: domains, dates, X handles, strategy/provider)",
        annotations(title = "Search", open_world_hint = true, read_only_hint = true)
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
        context: RequestContext<RoleServer>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.query.trim());
        let (token_name, request_id) =
            crate::log_request::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        if p.query.trim().is_empty() {
            let fields = crate::log_request::fields_from_meta(
                "/mcp/search",
                400,
                Some("ValidationError"),
                None,
                request_id.clone(),
                token_name,
                None,
                &serpotter_product::ExecMeta::default(),
            );
            crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
            return Ok(tool_error(
                "ValidationError",
                "missing query".to_string(),
                request_id,
            ));
        }
        let body = match search_params_to_query(p) {
            Ok(q) => q,
            Err(e) => {
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/search",
                    400,
                    Some("ValidationError"),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &serpotter_product::ExecMeta::default(),
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                return Ok(tool_error(
                    "ValidationError",
                    format!("invalid search params: {e}"),
                    request_id,
                ));
            }
        };
        // rmcp cancels this token when the client sends notifications/cancelled
        // for this request id; abort early instead of running to completion.
        let ct = context.ct.clone();
        let outcome = tokio::select! {
            r = serpotter_product::search_inner(&self.product, body) => r,
            _ = ct.cancelled() => {
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/search",
                    499,
                    Some("Cancelled"),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &serpotter_product::ExecMeta::default(),
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                return Ok(tool_error(
                    "Cancelled",
                    "request cancelled by client".to_string(),
                    request_id,
                ));
            }
        };
        match outcome {
            Ok(o) => {
                let resp = o.result;
                let exec_meta = o.meta;
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/search",
                    200,
                    None,
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    Some(resp.provider_used.clone()),
                    &exec_meta,
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                text_ok(resp, request_id)
            }
            Err(o) => {
                let e = o.result;
                let exec_meta = o.meta;
                let (status, kind) = search_err_log(&e);
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/search",
                    status,
                    Some(kind),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &exec_meta,
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                Ok(tool_error(kind, format!("search failed: {e}"), request_id))
            }
        }
    }

    #[tool(
        description = "Scrape/extract a URL (Firecrawl first, then Tavily fallback)",
        annotations(title = "Extract URL", open_world_hint = true, read_only_hint = true)
    )]
    async fn extract_url(
        &self,
        Parameters(p): Parameters<ExtractParams>,
        context: RequestContext<RoleServer>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.url.trim());
        let (token_name, request_id) =
            crate::log_request::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        if p.url.trim().is_empty() {
            let fields = crate::log_request::fields_from_meta(
                "/mcp/extract_url",
                400,
                Some("ValidationError"),
                None,
                request_id.clone(),
                token_name,
                None,
                &serpotter_product::ExecMeta::default(),
            );
            crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
            return Ok(tool_error(
                "ValidationError",
                "missing url".to_string(),
                request_id,
            ));
        }
        let ct = context.ct.clone();
        let outcome = tokio::select! {
            r = serpotter_product::extract_url(&self.product, p.url.trim(), p.provider.as_deref()) => r,
            _ = ct.cancelled() => {
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/extract_url",
                    499,
                    Some("Cancelled"),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &serpotter_product::ExecMeta::default(),
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                return Ok(tool_error(
                    "Cancelled",
                    "request cancelled by client".to_string(),
                    request_id,
                ));
            }
        };
        match outcome {
            Ok(o) => {
                let resp = o.result;
                let exec_meta = o.meta;
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/extract_url",
                    200,
                    None,
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    Some(resp.provider_used.clone()),
                    &exec_meta,
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                text_ok(resp, request_id)
            }
            Err(o) => {
                let e = o.result;
                let exec_meta = o.meta;
                let (status, kind) = extract_err_log(&e);
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/extract_url",
                    status,
                    Some(kind),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &exec_meta,
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                Ok(tool_error(kind, format!("extract failed: {e}"), request_id))
            }
        }
    }

    #[tool(
        description = "Deep research: search then scrape; response keys webResults, scrapedPages, optional socialResults; include_content for full page text. Soft progress when client sends _meta.progressToken.",
        annotations(title = "Research", open_world_hint = true, read_only_hint = true)
    )]
    async fn research(
        &self,
        Parameters(p): Parameters<ResearchParams>,
        context: RequestContext<RoleServer>,
        meta: Meta,
        peer: Peer<RoleServer>,
        Extension(parts): Extension<Parts>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.query.trim());
        let (token_name, request_id) =
            crate::log_request::resolve_mcp_log_ctx(&self.product.db, &parts).await;
        if p.query.trim().is_empty() {
            let fields = crate::log_request::fields_from_meta(
                "/mcp/research",
                400,
                Some("ValidationError"),
                None,
                request_id.clone(),
                token_name,
                None,
                &serpotter_product::ExecMeta::default(),
            );
            crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
            return Ok(tool_error(
                "ValidationError",
                "missing query".to_string(),
                request_id,
            ));
        }
        soft_progress(&peer, &meta, 0.0, 3.0, "research: starting").await;
        let body = ResearchRequest {
            query: p.query,
            web_max_results: p.web_max_results,
            scrape_top_n: p.scrape_top_n,
            include_content: p.include_content,
            social_max_results: p.social_max_results,
            include_domains: mcp_list_to_vec_or_one(p.include_domains),
            exclude_domains: mcp_list_to_vec_or_one(p.exclude_domains),
            allowed_x_handles: mcp_list_to_vec_or_one(p.allowed_x_handles),
            excluded_x_handles: mcp_list_to_vec_or_one(p.excluded_x_handles),
            from_date: p.from_date,
            to_date: p.to_date,
            time_range: p.time_range,
            country: p.country,
        };
        soft_progress(
            &peer,
            &meta,
            1.0,
            3.0,
            "research: running web/social/scrapes",
        )
        .await;
        let ct = context.ct.clone();
        let outcome = tokio::select! {
            r = serpotter_product::research_inner(&self.product, body) => r,
            _ = ct.cancelled() => {
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/research",
                    499,
                    Some("Cancelled"),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &serpotter_product::ExecMeta::default(),
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                return Ok(tool_error(
                    "Cancelled",
                    "request cancelled by client".to_string(),
                    request_id,
                ));
            }
        };
        match outcome {
            Ok(o) => {
                let resp = o.result;
                let exec_meta = o.meta;
                soft_progress(&peer, &meta, 3.0, 3.0, "research: complete").await;
                let provider_used = crate::log_request::research_dial_label(&exec_meta);
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/research",
                    200,
                    None,
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    provider_used,
                    &exec_meta,
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                text_ok(resp, request_id)
            }
            Err(o) => {
                let e = o.result;
                let exec_meta = o.meta;
                soft_progress(&peer, &meta, 3.0, 3.0, "research: failed").await;
                let (status, kind) = research_err_log(&e);
                let fields = crate::log_request::fields_from_meta(
                    "/mcp/research",
                    status,
                    Some(kind),
                    Some(preview),
                    request_id.clone(),
                    token_name,
                    None,
                    &exec_meta,
                );
                crate::log_request::spawn_log_db(self.product.db.clone(), fields, started);
                Ok(tool_error(
                    kind,
                    format!("research failed: {e}"),
                    request_id,
                ))
            }
        }
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

// rmcp-macros requires `version` to be a string literal, so the hard-coded
// value would drift from the crate. Omitting it makes rmcp emit
// `Implementation::new(name, env!("CARGO_PKG_VERSION"))` — serverInfo.version
// stays in sync with serpotter-api's crate version automatically.
#[tool_handler(
    router = self.tool_router,
    name = "serpotter",
    instructions = "Serpotter multi-provider search, extract, and research tools"
)]
impl ServerHandler for SerpotterMcp {}
