//! Serpotter HTTP API: search, extract, research, MCP, admin.

mod admin;
pub mod cron;
mod extract;
mod mcp;
mod mcp_session;
mod mcp_stream;
mod search;
mod log_request;

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_db::{Db, EXPECTED_SCHEMA_VERSION};
use serpotter_keypool::KeyPool;
use serpotter_providers::ProviderRegistry;

pub use admin::AdminState;
pub use extract::{ExtractRequest, ExtractResponse, ResearchRequest, ResearchResponse};
pub use mcp_session::{McpSessionStore, MCP_SESSION_HEADER, MCP_SESSION_TTL_SECS};
pub use search::SearchExecError;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub providers: ProviderRegistry,
    /// Optional bootstrap admin secret (ADMIN_SECRET env).
    pub admin_secret: Option<String>,
    /// Process-local MCP Streamable HTTP session registry.
    pub mcp_sessions: McpSessionStore,
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

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/api/search", post(search::search))
        .route("/api/extract", post(extract::extract_handler))
        .route("/api/research", post(extract::research_handler))
        .route(
            "/mcp",
            get(mcp_stream::mcp_get)
                .post(mcp::mcp_handler)
                .delete(mcp_stream::mcp_delete),
        )
        // Admin
        .route("/api/admin/bootstrap", post(admin::bootstrap))
        .route("/api/admin/login", post(admin::login))
        .route("/api/admin/logout", post(admin::logout))
        .route("/api/tokens", get(admin::list_tokens).post(admin::create_token))
        .route("/api/tokens/{id}", delete(admin::delete_token))
        .route("/api/keys", get(admin::list_keys).post(admin::create_key))
        .route("/api/keys/{id}", delete(admin::delete_key))
        .route("/api/keys/{id}/toggle", post(admin::toggle_key))
        .route("/api/keys/sync-credits", post(admin::sync_credits))
        .route(
            "/api/settings",
            get(admin::get_settings).put(admin::put_settings),
        )
        .route("/api/stats", get(admin::stats))
        .route("/api/nodes", get(admin::list_nodes).post(admin::create_node))
        .route("/api/nodes/{id}", delete(admin::delete_node))
        .with_state(state)
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
                status: "ok",
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

/// Require a valid API token (Bearer or x-api-key). Returns problem response on failure.
#[allow(clippy::result_large_err)]
pub async fn require_api_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
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
