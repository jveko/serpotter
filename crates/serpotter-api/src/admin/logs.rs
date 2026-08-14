//! Admin request_log browser.

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::require_admin;
use crate::events::{RingEntryView, RingFilter};
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
    let limit = q.limit.unwrap_or(50).clamp(1, 200) as usize;
    // Lenient status filter: non-numeric values (e.g. "2xx") are treated as
    // absent rather than a 400 so dashboards can pass through raw inputs.
    let status = q.status.and_then(|s| s.parse::<i64>().ok());
    let offset = q.offset.unwrap_or(0).max(0) as usize;
    let filter = RingFilter {
        limit,
        offset,
        status,
        path_prefix: q.path,
        service: q.service,
        request_id: q.request_id,
        token_name: q.token_name,
    };
    let views = state.events.ring.list(&filter);
    let out: Vec<LogOut> = views.into_iter().map(log_out_from_view).collect();
    (StatusCode::OK, Json(out)).into_response()
}

fn log_out_from_view(v: RingEntryView) -> LogOut {
    let f = v.fields;
    LogOut {
        id: v.id,
        created_at: v.created_at,
        path: f.path.to_string(),
        method: "POST".to_string(),
        status: f.status,
        service: f.service,
        provider_used: f.provider_used,
        duration_ms: f.duration_ms,
        error_kind: f.error_kind.map(str::to_string),
        query_preview: f.query_preview,
        request_id: f.request_id,
        token_name: f.token_name,
        strategy: f.strategy,
        providers_consulted: f.providers_consulted,
        attempt_count: f.attempt_count,
        key_id: f.key_id,
        node_id: f.node_id,
    }
}
