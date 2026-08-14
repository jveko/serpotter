//! Admin stats handler.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use serpotter_auth::problem_response;

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
    recent_requests: i64,
    by_service: Vec<ServiceStatsOut>,
}

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    // Fail closed on DbErr — soft-zeroing hid real outages behind an empty 200 dashboard.
    let tokens = match ctx.db.count_tokens().await {
        Ok(n) => n,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    let api_keys = match ctx.db.count_api_keys().await {
        Ok(n) => n,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    let active_api_keys = match ctx.db.count_active_api_keys().await {
        Ok(n) => n,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    let nodes = match ctx.db.count_nodes().await {
        Ok(n) => n,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    let schema_version = match ctx.db.schema_version().await {
        Ok(n) => n,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    // In-memory ring length: the request-log surface is no longer a DB table.
    let recent_requests = state.events.ring.len() as i64;
    let by_service = match ctx.db.stats_by_service().await {
        Ok(rows) => rows
            .into_iter()
            .map(|s| ServiceStatsOut {
                service: s.service,
                keys: s.keys,
                active: s.active,
                credits_remaining: s.credits_remaining_sum,
                credits_limit: s.credits_limit_sum,
            })
            .collect(),
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    let out = StatsOut {
        tokens,
        api_keys,
        active_api_keys,
        nodes,
        schema_version,
        recent_requests,
        by_service,
    };
    (StatusCode::OK, Json(out)).into_response()
}
