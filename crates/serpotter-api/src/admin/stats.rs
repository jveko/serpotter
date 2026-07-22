//! Admin stats handler.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

use super::require_admin;
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServiceStatsOut {
    service: String,
    keys: i64,
    active: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    credits_remaining: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credits_limit: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsOut {
    tokens: i64,
    api_keys: i64,
    active_api_keys: i64,
    nodes: i64,
    schema_version: i64,
    request_logs: i64,
    by_service: Vec<ServiceStatsOut>,
}

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }
    let tokens = state.db.count_tokens().await.unwrap_or(0);
    let api_keys = state.db.count_api_keys().await.unwrap_or(0);
    let active_api_keys = state.db.count_active_api_keys().await.unwrap_or(0);
    let nodes = state.db.count_nodes().await.unwrap_or(0);
    let schema_version = state.db.schema_version().await.unwrap_or(0);
    let request_logs = state.db.count_request_logs().await.unwrap_or(0);
    let by_service = state
        .db
        .stats_by_service()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| ServiceStatsOut {
            service: s.service,
            keys: s.keys,
            active: s.active,
            credits_remaining: s.credits_remaining_sum,
            credits_limit: s.credits_limit_sum,
        })
        .collect();
    let out = StatsOut {
        tokens,
        api_keys,
        active_api_keys,
        nodes,
        schema_version,
        request_logs,
        by_service,
    };
    (StatusCode::OK, Json(out)).into_response()
}
