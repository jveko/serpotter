//! MCP Streamable HTTP via official `rmcp` SDK.
//!
//! Tool args accept mysearch snake_case (preferred) and camelCase aliases.
//! Auth is outer axum middleware (Bearer / x-api-key) — session ≠ authentication.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{schemars, tool, tool_handler, tool_router, ServerHandler};
use serde::Deserialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_core::SearchQuery;
use serpotter_db::EXPECTED_SCHEMA_VERSION;
use serpotter_product::{ProductCtx, ResearchRequest};

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

async fn mcp_auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if let Err(r) = require_mcp_token(&state, request.headers()).await {
        return r;
    }
    next.run(request).await
}

async fn require_mcp_token(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Result<(), Response> {
    let Some(token) = extract_token(headers) else {
        return Err(authentication_error("Missing API token"));
    };
    match state.db.get_token_by_value(&token).await {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(authentication_error("Invalid token")),
        Err(_) => Err(problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            "Token lookup failed",
        )),
    }
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

// --- tool param DTOs (snake_case fields + camelCase serde aliases) ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchParams {
    #[schemars(description = "Search query string")]
    query: String,
    #[serde(default, alias = "maxResults")]
    #[schemars(description = "Max results (1–20)")]
    max_results: Option<u32>,
    #[serde(default, alias = "includeContent")]
    include_content: Option<bool>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExtractParams {
    #[schemars(description = "URL to extract")]
    url: String,
    #[serde(default)]
    provider: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ResearchParams {
    #[schemars(description = "Research query")]
    query: String,
    #[serde(default, alias = "webMaxResults", alias = "max_results", alias = "maxResults")]
    #[schemars(description = "Web search result cap")]
    web_max_results: Option<u32>,
    #[serde(default, alias = "socialMaxResults")]
    social_max_results: Option<u32>,
    #[serde(
        default,
        alias = "scrapeTopN",
        alias = "extract_top_n",
        alias = "extractTopN"
    )]
    scrape_top_n: Option<u32>,
}

#[tool_router]
impl SerpotterMcp {
    #[tool(description = "Web search via multi-provider routing")]
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
        let body = SearchQuery {
            query: p.query,
            max_results: p.max_results,
            include_content: p.include_content,
            mode: p.mode,
            provider: p.provider,
            ..Default::default()
        };
        match serpotter_product::search_inner(&self.product, body).await {
            Ok(resp) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp",
                    200,
                    Some(resp.provider_used.clone()),
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp",
                    502,
                    None,
                    Some("ToolError"),
                    Some(preview),
                    started,
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "search failed: {e}"
                ))]))
            }
        }
    }

    #[tool(description = "Scrape/extract a URL (Firecrawl then Tavily)")]
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
                    "/mcp",
                    200,
                    None,
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp",
                    502,
                    None,
                    Some("ToolError"),
                    Some(preview),
                    started,
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "extract failed: {e}"
                ))]))
            }
        }
    }

    #[tool(description = "Search then scrape top results (mysearch research tool)")]
    async fn research(
        &self,
        Parameters(p): Parameters<ResearchParams>,
    ) -> Result<CallToolResult, rmcp::ErrorData> {
        let started = Instant::now();
        let preview = crate::log_request::query_preview(p.query.trim());
        if p.query.trim().is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "missing query",
            )]));
        }
        let body = ResearchRequest {
            query: p.query,
            web_max_results: p.web_max_results,
            scrape_top_n: p.scrape_top_n,
            include_content: None,
            social_max_results: p.social_max_results,
        };
        match serpotter_product::research_inner(&self.product, body).await {
            Ok(resp) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp",
                    200,
                    None,
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp",
                    502,
                    None,
                    Some("ToolError"),
                    Some(preview),
                    started,
                );
                Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "research failed: {e}"
                ))]))
            }
        }
    }

    #[tool(name = "mysearch_health", description = "Health and schema version")]
    async fn mysearch_health(&self) -> Result<CallToolResult, rmcp::ErrorData> {
        let version = self.product.db.schema_version().await.ok();
        let body = serde_json::json!({
            "status": "ok",
            "schemaVersion": version,
            "expected": self.expected_schema_version,
        });
        Ok(CallToolResult::success(vec![ContentBlock::text(
            body.to_string(),
        )]))
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "serpotter",
    version = "0.1.0",
    instructions = "Serpotter multi-provider search, extract, and research tools"
)]
impl ServerHandler for SerpotterMcp {}

fn text_ok<T: serde::Serialize>(value: T) -> Result<CallToolResult, rmcp::ErrorData> {
    match serde_json::to_string_pretty(&value) {
        Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
        Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "serialize failed: {e}"
        ))])),
    }
}
