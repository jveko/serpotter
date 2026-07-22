use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_db::{Db, EXPECTED_SCHEMA_VERSION};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StubBody {
    status: &'static str,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/api/search", post(search_stub))
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

async fn search_stub(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let Some(token) = extract_token(&headers) else {
        return authentication_error("Missing API token");
    };

    match state.db.get_token_by_value(&token).await {
        Ok(Some(_)) => (
            StatusCode::NOT_IMPLEMENTED,
            Json(StubBody {
                status: "not_implemented",
            }),
        )
            .into_response(),
        Ok(None) => authentication_error("Invalid token"),
        Err(_) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            "Token lookup failed",
        ),
    }
}
