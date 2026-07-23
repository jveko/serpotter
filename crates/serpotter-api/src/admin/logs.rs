//! Admin request_log browser.

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
pub struct ListLogsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LogOut {
    id: i64,
    created_at: String,
    path: String,
    method: String,
    status: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_used: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_preview: Option<String>,
}

pub async fn list_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListLogsQuery>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let limit = q.limit.unwrap_or(50);
    match ctx.db.list_request_logs(limit).await {
        Ok(rows) => {
            let out: Vec<LogOut> = rows
                .into_iter()
                .map(|r| LogOut {
                    id: r.id,
                    created_at: r.created_at,
                    path: r.path,
                    method: r.method,
                    status: r.status,
                    service: r.service,
                    provider_used: r.provider_used,
                    duration_ms: r.duration_ms,
                    error_kind: r.error_kind,
                    query_preview: r.query_preview,
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
