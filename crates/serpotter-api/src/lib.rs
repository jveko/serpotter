use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde::Serialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_db::{Db, EXPECTED_SCHEMA_VERSION};
use serpotter_keypool::{KeyPool, KeyPoolError};
use serpotter_tavily::{TavilyClient, TavilyError, TavilySearchParams};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub tavily: TavilyClient,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub query: String,
    pub max_results: Option<u32>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/api/search", post(search))
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

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SearchQuery>,
) -> impl IntoResponse {
    let Some(token) = extract_token(&headers) else {
        return authentication_error("Missing API token");
    };

    match state.db.get_token_by_value(&token).await {
        Ok(Some(_)) => {}
        Ok(None) => return authentication_error("Invalid token"),
        Err(_) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                "Token lookup failed",
            );
        }
    }

    let query = body.query.trim();
    if query.is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_query");
    }

    let max_results = body.max_results.unwrap_or(5).clamp(1, 20);

    let lease = match state.keys.acquire(serpotter_tavily::SERVICE).await {
        Ok(l) => l,
        Err(KeyPoolError::NoHealthyKey(_)) => {
            return problem_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "NoHealthyKey",
                "No healthy tavily key",
            );
        }
        Err(KeyPoolError::Db(_)) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                "Key acquire failed",
            );
        }
    };

    let result = state
        .tavily
        .search(TavilySearchParams {
            query,
            max_results,
            api_key: &lease.key,
            search_depth: "basic",
            include_answer: true,
        })
        .await;

    match result {
        Ok(resp) => {
            let _ = state.keys.report_success(lease.id).await;
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(TavilyError::Upstream { status, body }) if status == 429 || (500..600).contains(&status) => {
            let _ = state.keys.report_failure(lease.id).await;
            problem_response(
                StatusCode::BAD_GATEWAY,
                "ProviderError",
                format!("tavily upstream {status}: {body}"),
            )
        }
        Err(TavilyError::Upstream { status, body }) => {
            // client errors (4xx except 429) don't burn consecutive fails the same way;
            // still report failure once so bad keys surface.
            if status == 401 || status == 403 {
                let _ = state.keys.report_failure(lease.id).await;
            }
            problem_response(
                StatusCode::BAD_GATEWAY,
                "ProviderError",
                format!("tavily upstream {status}: {body}"),
            )
        }
        Err(TavilyError::Http(e)) => {
            let _ = state.keys.report_failure(lease.id).await;
            problem_response(
                StatusCode::BAD_GATEWAY,
                "SearchError",
                format!("tavily request failed: {e}"),
            )
        }
    }
}
