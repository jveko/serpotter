//! MCP Streamable HTTP via official `rmcp` SDK.
//!
//! Tool args accept snake_case (preferred) and camelCase aliases.
//! Auth is outer axum middleware (Bearer / x-api-key) — session ≠ authentication.

mod auth;
mod params;
mod progress;

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use axum::middleware;
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Meta};
use rmcp::service::{Peer, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};
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
pub fn service(state: AppState) -> impl tower::Service<
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
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.query.trim());
        if p.query.trim().is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "missing query",
            )]));
        }
        let body = match search_params_to_query(p) {
            Ok(q) => q,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "invalid search params: {e}"
                ))]));
            }
        };
        match serpotter_product::search_inner(&self.product, body).await {
            Ok(resp) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/search",
                    200,
                    Some(resp.provider_used.clone()),
                    Some(resp.provider_used.clone()),
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                let (status, kind) = search_err_log(&e);
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/search",
                    status,
                    None,
                    None,
                    Some(kind),
                    Some(preview),
                    started,
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "search failed: {e}"
                ))]))
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
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.url.trim());
        if p.url.trim().is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "missing url",
            )]));
        }
        match serpotter_product::extract_url(
            &self.product,
            p.url.trim(),
            p.provider.as_deref(),
        )
        .await
        {
            Ok(resp) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/extract_url",
                    200,
                    Some(resp.provider_used.clone()),
                    Some(resp.provider_used.clone()),
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                let (status, kind) = extract_err_log(&e);
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/extract_url",
                    status,
                    None,
                    None,
                    Some(kind),
                    Some(preview),
                    started,
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "extract failed: {e}"
                ))]))
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
        meta: Meta,
        peer: Peer<RoleServer>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.query.trim());
        if p.query.trim().is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "missing query",
            )]));
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
        match serpotter_product::research_inner(&self.product, body).await {
            Ok(resp) => {
                soft_progress(&peer, &meta, 3.0, 3.0, "research: complete").await;
                let provider_used = resp
                    .evidence
                    .as_ref()
                    .and_then(|e| e.providers_consulted.as_ref())
                    .and_then(|p| p.first())
                    .cloned();
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/research",
                    200,
                    provider_used.clone(),
                    provider_used,
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                soft_progress(&peer, &meta, 3.0, 3.0, "research: failed").await;
                let (status, kind) = research_err_log(&e);
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/research",
                    status,
                    None,
                    None,
                    Some(kind),
                    Some(preview),
                    started,
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "research failed: {e}"
                ))]))
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

// serverInfo.version is a string literal (rmcp-macros); keep in sync with crate version.
#[tool_handler(
    router = self.tool_router,
    name = "serpotter",
    version = "0.1.0",
    instructions = "Serpotter multi-provider search, extract, and research tools"
)]
impl ServerHandler for SerpotterMcp {}
