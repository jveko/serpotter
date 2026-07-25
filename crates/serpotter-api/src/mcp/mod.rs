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
use serpotter_product::{
    ExtractError, ProductCtx, ResearchError, ResearchRequest, SearchExecError,
};

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
#[serde(untagged)]
enum McpStringList {
    One(String),
    Many(Vec<String>),
}

impl McpStringList {
    fn into_json(self) -> serde_json::Value {
        match self {
            Self::One(s) => serde_json::Value::String(s),
            Self::Many(v) => serde_json::Value::Array(
                v.into_iter().map(serde_json::Value::String).collect(),
            ),
        }
    }
}

/// Map MCP list field into core `VecOrOne` via SearchQuery's camelCase serde.
fn mcp_list_field(list: Option<McpStringList>) -> Option<serde_json::Value> {
    list.map(McpStringList::into_json)
}

fn mcp_list_to_vec_or_one(list: Option<McpStringList>) -> Option<serpotter_core::VecOrOne> {
    match list {
        None => None,
        Some(McpStringList::One(s)) => Some(serpotter_core::VecOrOne::One(s)),
        Some(McpStringList::Many(v)) => Some(serpotter_core::VecOrOne::Many(v)),
    }
}

fn search_params_to_query(p: SearchParams) -> Result<SearchQuery, String> {
    let v = serde_json::json!({
        "query": p.query,
        "maxResults": p.max_results,
        "mode": p.mode,
        "intent": p.intent,
        "strategy": p.strategy,
        "provider": p.provider,
        "sources": p.sources.map(McpStringList::into_json),
        "includeContent": p.include_content,
        "includeDomains": mcp_list_field(p.include_domains),
        "excludeDomains": mcp_list_field(p.exclude_domains),
        "allowedXHandles": mcp_list_field(p.allowed_x_handles),
        "excludedXHandles": mcp_list_field(p.excluded_x_handles),
        "fromDate": p.from_date,
        "toDate": p.to_date,
        "searchDepth": p.search_depth,
        "timeRange": p.time_range,
        "country": p.country,
        "exactMatch": p.exact_match,
    });
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SearchParams {
    #[schemars(description = "Search query string")]
    query: String,
    #[serde(default, alias = "maxResults")]
    #[schemars(description = "Max results (1–20)")]
    max_results: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Search mode (auto, web, news, social, docs, research, github, pdf)")]
    mode: Option<String>,
    #[serde(default)]
    #[schemars(description = "Query intent (auto, factual, status, comparison, tutorial, exploratory, news, resource)")]
    intent: Option<String>,
    #[serde(default)]
    #[schemars(description = "Routing strategy (auto, fast, balanced, verify, deep)")]
    strategy: Option<String>,
    #[serde(default)]
    #[schemars(description = "Force a specific provider (tavily, firecrawl, exa, xai, auto)")]
    provider: Option<String>,
    #[serde(default)]
    #[schemars(description = "Source filter: \"web\", \"x\", or a list of those")]
    sources: Option<McpStringList>,
    #[serde(default, alias = "includeContent")]
    #[schemars(description = "Include full page content in results when supported")]
    include_content: Option<bool>,
    #[serde(default, alias = "includeDomains")]
    #[schemars(description = "Only include results from these domains (string or list)")]
    include_domains: Option<McpStringList>,
    #[serde(default, alias = "excludeDomains")]
    #[schemars(description = "Exclude results from these domains (string or list)")]
    exclude_domains: Option<McpStringList>,
    #[serde(default, alias = "allowedXHandles")]
    #[schemars(description = "X/Twitter: only these handles (string or list)")]
    allowed_x_handles: Option<McpStringList>,
    #[serde(default, alias = "excludedXHandles")]
    #[schemars(description = "X/Twitter: exclude these handles (string or list)")]
    excluded_x_handles: Option<McpStringList>,
    #[serde(default, alias = "fromDate")]
    #[schemars(description = "Lower bound date filter (YYYY-MM-DD or relative)")]
    from_date: Option<String>,
    #[serde(default, alias = "toDate")]
    #[schemars(description = "Upper bound date filter (YYYY-MM-DD or relative)")]
    to_date: Option<String>,
    #[serde(default, alias = "searchDepth")]
    #[schemars(description = "Tavily search_depth: basic, advanced, fast, ultra-fast")]
    search_depth: Option<String>,
    #[serde(default, alias = "timeRange")]
    #[schemars(description = "Relative time range: day, week, month, year")]
    time_range: Option<String>,
    #[serde(default)]
    #[schemars(description = "Country bias / locale hint for providers that support it")]
    country: Option<String>,
    #[serde(default, alias = "exactMatch")]
    #[schemars(description = "Prefer exact phrase matching when supported")]
    exact_match: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ExtractParams {
    #[schemars(description = "URL to extract")]
    url: String,
    #[serde(default)]
    #[schemars(description = "Preferred extract provider (firecrawl, tavily)")]
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
    #[schemars(description = "Social/X result cap (0 disables)")]
    social_max_results: Option<u32>,
    #[serde(
        default,
        alias = "scrapeTopN",
        alias = "extract_top_n",
        alias = "extractTopN"
    )]
    #[schemars(description = "How many top search hits to scrape (0–10)")]
    scrape_top_n: Option<u32>,
    #[serde(default, alias = "includeContent")]
    #[schemars(description = "Include full page content in scraped results when supported")]
    include_content: Option<bool>,
    #[serde(default, alias = "includeDomains")]
    #[schemars(description = "Only include results from these domains (string or list)")]
    include_domains: Option<McpStringList>,
    #[serde(default, alias = "excludeDomains")]
    #[schemars(description = "Exclude results from these domains (string or list)")]
    exclude_domains: Option<McpStringList>,
    #[serde(default, alias = "allowedXHandles")]
    #[schemars(description = "X/Twitter: only these handles (string or list)")]
    allowed_x_handles: Option<McpStringList>,
    #[serde(default, alias = "excludedXHandles")]
    #[schemars(description = "X/Twitter: exclude these handles (string or list)")]
    excluded_x_handles: Option<McpStringList>,
    #[serde(default, alias = "fromDate")]
    #[schemars(description = "Lower bound date filter (YYYY-MM-DD or relative)")]
    from_date: Option<String>,
    #[serde(default, alias = "toDate")]
    #[schemars(description = "Upper bound date filter (YYYY-MM-DD or relative)")]
    to_date: Option<String>,
    #[serde(default, alias = "timeRange")]
    #[schemars(description = "Relative time range: day, week, month, year")]
    time_range: Option<String>,
    #[serde(default)]
    #[schemars(description = "Country bias / locale hint")]
    country: Option<String>,
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
        description = "Deep research: search then scrape; response keys webResults, scrapedPages, optional socialResults; include_content for full page text",
        annotations(title = "Research", open_world_hint = true, read_only_hint = true)
    )]
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
        match serpotter_product::research_inner(&self.product, body).await {
            Ok(resp) => {
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
                    provider_used,
                    None,
                    Some(preview),
                    started,
                );
                text_ok(resp)
            }
            Err(e) => {
                let (status, kind) = research_err_log(&e);
                crate::log_request::spawn_log_db(
                    self.product.db.clone(),
                    "/mcp/research",
                    status,
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
        name = "mysearch_health",
        description = "Readiness and schema version (schemaVersion vs expected)",
        annotations(title = "Health", read_only_hint = true, open_world_hint = false)
    )]
    async fn mysearch_health(&self) -> Result<CallToolResult, rmcp::ErrorData> {
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

fn search_err_log(e: &SearchExecError) -> (i64, &'static str) {
    match e {
        SearchExecError::NoHealthyKey(_) => (503, "NoHealthyKey"),
        SearchExecError::KeyBusy(_) => (503, "KeyBusy"),
        SearchExecError::NoHealthyNode(_) => (503, "NoHealthyNode"),
        SearchExecError::Provider(_) => (502, "ProviderError"),
        SearchExecError::Search(_) => (502, "SearchError"),
        SearchExecError::Db(_) => (500, "DatabaseError"),
    }
}

fn extract_err_log(e: &ExtractError) -> (i64, &'static str) {
    match e {
        ExtractError::NoHealthyKey(_) => (503, "NoHealthyKey"),
        ExtractError::KeyBusy(_) => (503, "KeyBusy"),
        ExtractError::NoHealthyNode(_) => (503, "NoHealthyNode"),
        ExtractError::InvalidUrl(_) => (400, "ValidationError"),
        ExtractError::Provider(_) => (502, "ProviderError"),
        ExtractError::Db(_) => (500, "DatabaseError"),
    }
}

fn research_err_log(e: &ResearchError) -> (i64, &'static str) {
    match e {
        ResearchError::Search(s) => search_err_log(s),
        ResearchError::Extract(x) => extract_err_log(x),
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

fn text_ok<T: serde::Serialize>(value: T) -> Result<CallToolResult, rmcp::ErrorData> {
    match serde_json::to_string_pretty(&value) {
        Ok(s) => Ok(CallToolResult::success(vec![ContentBlock::text(s)])),
        Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
            "serialize failed: {e}"
        ))])),
    }
}
