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
    #[serde(default)]
    pub offset: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub token_name: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    providers_consulted: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<i64>,
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
    // Lenient status filter: non-numeric values (e.g. "2xx") are treated as
    // absent rather than a 400 so dashboards can pass through raw inputs.
    let status = q.status.and_then(|s| s.parse::<i64>().ok());
    let filter = serpotter_db::RequestLogFilter {
        limit,
        offset: q.offset.unwrap_or(0),
        status,
        path_prefix: q.path,
        service: q.service,
        request_id: q.request_id,
        token_name: q.token_name,
    };
    match ctx.db.list_request_logs(filter).await {
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
                    request_id: r.request_id,
                    token_name: r.token_name,
                    strategy: r.strategy,
                    providers_consulted: r.providers_consulted,
                    attempt_count: r.attempt_count,
                    key_id: r.key_id,
                    node_id: r.node_id,
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
