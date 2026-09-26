//! Usage dashboard: daily usage summary + spend by key/service.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_db::clamp_usage_days;

use super::extract::database_problem;
use super::extract::AppQuery;
use super::require_admin;
use crate::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageQuery {
    #[serde(default)]
    pub days: Option<i64>,
}

/// `days` window for the spend endpoints. `usage_daily` has no retention job,
/// so the aggregates are windowed like `/api/usage` (default 90d, clamped
/// 1..=180) and the DB layer caps the grouped row count.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendQuery {
    #[serde(default)]
    pub days: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageDailyOut {
    service: String,
    provider_used: String,
    date: String,
    requests: i64,
    successes: i64,
    errors: i64,
    tokens: i64,
    cost: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpendKeyOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_name: Option<String>,
    service: String,
    requests: i64,
    cost: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpendServiceOut {
    service: String,
    requests: i64,
    cost: f64,
}

/// GET /api/usage?days=N — daily request/token/cost per service+provider from
/// usage_daily (accumulated at write time by the request-events usage writer).
/// Days default 14, clamped through the shared `clamp_usage_days` (1..=180 so
/// the dashboard's current+previous window pattern works at its 90d setting).
pub async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppQuery(q): AppQuery<UsageQuery>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let days = clamp_usage_days(q.days.unwrap_or(14));
    match ctx.db.usage_summary(days).await {
        Ok(rows) => {
            let out: Vec<UsageDailyOut> = rows
                .into_iter()
                .map(|r| UsageDailyOut {
                    service: r.service,
                    provider_used: r.provider_used,
                    date: r.date,
                    requests: r.requests,
                    successes: r.successes,
                    errors: r.errors,
                    tokens: r.tokens,
                    cost: r.cost,
                })
                .collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}

/// GET /api/spend/keys?days=N — cost + request count per API key (joined to
/// api_keys for the service; 'unknown' when the key row is gone), ordered by
/// spend. `days` defaults to 90, clamped 1..=180 (shared `clamp_usage_days`).
pub async fn spend_by_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppQuery(q): AppQuery<SpendQuery>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let days = clamp_usage_days(q.days.unwrap_or(90));
    match ctx.db.spend_by_key(days).await {
        Ok(rows) => {
            let out: Vec<SpendKeyOut> = rows
                .into_iter()
                .map(|r| SpendKeyOut {
                    key_id: r.key_id,
                    token_name: r.token_name,
                    service: r.service,
                    requests: r.requests,
                    cost: r.cost,
                })
                .collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}

/// GET /api/spend/services?days=N — cost + request count per service, ordered
/// by spend. `days` defaults to 90, clamped 1..=180 (shared `clamp_usage_days`).
pub async fn spend_by_services(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppQuery(q): AppQuery<SpendQuery>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let days = clamp_usage_days(q.days.unwrap_or(90));
    match ctx.db.spend_by_service(days).await {
        Ok(rows) => {
            let out: Vec<SpendServiceOut> = rows
                .into_iter()
                .map(|r| SpendServiceOut {
                    service: r.service,
                    requests: r.requests,
                    cost: r.cost,
                })
                .collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}
