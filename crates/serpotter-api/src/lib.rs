//! Serpotter HTTP API: search, extract, research, MCP, admin.

mod admin;
mod credit_sync;
pub mod cron;
mod log_request;
mod mcp;
mod product;
pub mod trace_layer;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, FromRequestParts, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_db::{Db, EXPECTED_SCHEMA_VERSION};
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_product::ProductCtx;
use serpotter_providers::ProviderRegistry;

pub use admin::AdminCtx;
pub use mcp::{MCP_SESSION_HEADER, MCP_SESSION_TTL_SECS};
pub use serpotter_product::{
    ExtractRequest, ExtractResponse, ResearchRequest, ResearchResponse, SearchExecError,
};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub outbound: Arc<ProxyPool>,
    pub providers: ProviderRegistry,
    /// Optional bootstrap admin secret (ADMIN_SECRET env).
    pub admin_secret: Option<String>,
}

impl AppState {
    pub fn product_ctx(&self) -> ProductCtx {
        ProductCtx {
            db: self.db.clone(),
            keys: self.keys.clone(),
            outbound: self.outbound.clone(),
            providers: self.providers.clone(),
            progress: None,
            // F10: overall per-request deadline. Read at ctx-build time
            // (once per product request); invalid values warn + default 120s.
            request_timeout: product::request_timeout_from_env(),
        }
    }

    pub fn admin_ctx(&self) -> AdminCtx {
        AdminCtx {
            db: self.db.clone(),
            providers: self.providers.clone(),
            admin_secret: self.admin_secret.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveBody {
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyBody {
    status: &'static str,
    schema_version: Option<i64>,
    expected: i64,
}

/// Explicit inbound body limit (2 MiB). Matches typical axum default; set deliberately.
pub const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Build the router, reading the SPA directory from `ADMIN_SPA_DIR`.
pub fn app(state: AppState) -> Router {
    let spa_dir = std::env::var("ADMIN_SPA_DIR").ok();
    app_with_spa(state, spa_dir.as_deref())
}

/// Same as [`app`], with the SPA directory passed explicitly instead of read
/// from the environment. Lets tests exercise SPA routing without touching
/// process-global env state.
pub fn app_with_spa(state: AppState, spa_dir: Option<&str>) -> Router {
    let mut router = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/api/search", post(product::search::search))
        .route("/api/extract", post(product::extract::extract_handler))
        .route("/api/research", post(product::extract::research_handler))
        .nest_service("/mcp", mcp::service(state.clone()))
        // Admin
        .route("/api/admin/bootstrap", post(admin::bootstrap))
        .route("/api/admin/login", post(admin::login))
        .route("/api/admin/logout", post(admin::logout))
        .route(
            "/api/tokens",
            get(admin::list_tokens).post(admin::create_token),
        )
        .route("/api/tokens/{id}", delete(admin::delete_token))
        .route("/api/keys", get(admin::list_keys).post(admin::create_key))
        .route(
            "/api/keys/{id}",
            delete(admin::delete_key).put(admin::update_key),
        )
        .route("/api/keys/{id}/toggle", post(admin::toggle_key))
        .route("/api/keys/sync-credits", post(admin::sync_credits))
        .route(
            "/api/settings",
            get(admin::get_settings).put(admin::put_settings),
        )
        .route("/api/stats", get(admin::stats))
        .route("/api/request-logs", get(admin::list_request_logs))
        .route(
            "/api/nodes",
            get(admin::list_nodes).post(admin::create_node),
        )
        .route(
            "/api/nodes/{id}",
            delete(admin::delete_node).put(admin::update_node),
        )
        .route("/api/nodes/{id}/toggle", post(admin::toggle_node))
        // Unknown /api paths answer a JSON problem, never the SPA's index.html.
        // Without this the root SPA fallback below would serve HTML with 200 to
        // a mistyped endpoint, which is far harder to debug than a 404.
        .route("/api", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .with_state(state)
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES));

    // Optional static SPA at the site root: ADMIN_SPA_DIR=/path/to/apps/admin/dist.
    // ServeDir resolves real files (/assets/*, /favicon.ico); anything it cannot
    // find falls back to index.html, so refreshing a client route (/keys, /logs)
    // boots the app instead of 404ing. Registered as the router's fallback, so
    // every route declared above — /api, /mcp, /live, /ready — still wins.
    if let Some(dir) = spa_dir.map(str::trim).filter(|d| !d.is_empty()) {
        let index = std::path::Path::new(dir).join("index.html");
        let spa = tower_http::services::ServeDir::new(dir)
            .fallback(tower_http::services::ServeFile::new(index));
        router = router.fallback_service(spa);
    }

    // Request-id + trace stack, applied after the SPA fallback so every
    // response (API routes and SPA static files) carries a bounded
    // x-request-id. Layer order, last added = outermost (axum wraps each new
    // layer around the previous): `bound_request_id` (outermost) ->
    // SetRequestIdLayer -> TraceLayer -> PropagateRequestIdLayer (innermost).
    // The bound middleware truncates an oversized inbound x-request-id to
    // MAX_REQUEST_ID_LEN bytes *before* the set/trace/propagate layers see it,
    // so spans, request_log rows, and the propagated response header all
    // observe the bounded id. Wired here (inside `app_with_spa`) so the
    // production stack and the integration-test stack are identical; `main.rs`
    // adds no layers of its own.
    let (set_request_id, trace, propagate) = trace_layer::build_http_layers();
    router
        .layer(propagate)
        .layer(trace)
        .layer(set_request_id)
        .layer(axum::middleware::from_fn(trace_layer::bound_request_id))
}

async fn api_not_found() -> axum::response::Response {
    problem_response(StatusCode::NOT_FOUND, "NotFound", "Unknown API endpoint")
}

async fn live() -> Json<LiveBody> {
    Json(LiveBody { status: "ok" })
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let expected = EXPECTED_SCHEMA_VERSION;
    match state.db.schema_version().await {
        Ok(version) if version >= expected => (
            StatusCode::OK,
            Json(ReadyBody {
                status: "ready",
                schema_version: Some(version),
                expected,
            }),
        )
            .into_response(),
        Ok(version) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyBody {
                status: "not_ready",
                schema_version: Some(version),
                expected,
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyBody {
                status: "not_ready",
                schema_version: None,
                expected,
            }),
        )
            .into_response(),
    }
}

/// Require a valid API token (Bearer or x-api-key). Returns the token row on success.
#[allow(clippy::result_large_err)]
pub async fn require_api_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<serpotter_db::TokenRow, axum::response::Response> {
    let Some(token) = extract_token(headers) else {
        return Err(authentication_error("Missing API token"));
    };
    match state.db.get_token_by_value(&token).await {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(authentication_error("Invalid token")),
        Err(_) => Err(problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            "Token lookup failed",
        )),
    }
}

/// Parts-level API-token extractor (F01): runs [`require_api_token`] before
/// any body extractor, so an unauthenticated request answers 401 even when
/// the JSON body is malformed or missing. Axum runs all `FromRequestParts`
/// extractors before the single body `FromRequest` extractor, so ordering
/// `ApiToken` before `AppJson` in a handler signature makes auth win over
/// body deserialization.
pub struct ApiToken(pub serpotter_db::TokenRow);

#[allow(clippy::result_large_err)]
impl FromRequestParts<AppState> for ApiToken {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        require_api_token(state, &parts.headers).await.map(ApiToken)
    }
}
