//! Usage dashboard: daily usage summary + spend by key/service.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;

use super::require_admin;
use crate::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageQuery {
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
/// usage_daily (accumulated at write time by the request-events usage writer). Days default 14, clamp 1..=180
/// (180 so the admin dashboard's current+previous window pattern works at its 90d setting).
pub async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UsageQuery>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let days = q.days.unwrap_or(14).clamp(1, 180);
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
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// GET /api/spend/keys — cost + request count per API key (joined to api_keys
/// for the service; 'unknown' when the key row is gone), ordered by spend.
pub async fn spend_by_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.spend_by_key().await {
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
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// GET /api/spend/services — cost + request count per service, ordered by spend.
pub async fn spend_by_services(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.spend_by_service().await {
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
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
