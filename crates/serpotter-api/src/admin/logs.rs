//! Admin browser for the in-memory request-events ring.
//!
//! Migration 0017 dropped the `request_log` table; the durable audit lives in
//! the JSON log stream and this handler reads the 2,048-entry in-memory ring
//! in `crate::events` (newest-first, lost on restart). See `docs/ops/api.md`.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use super::extract::AppQuery;
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
    /// Exact `errorKind` match (e.g. `Timeout`, `Unauthorized`) — the field is
    /// already on every ring row, it just had no filter.
    #[serde(default)]
    pub error_kind: Option<String>,
    /// Exact `lastUpstreamStatus` match (the upstream status the request's
    /// last provider attempt saw). Lenient like `status`: a non-numeric value
    /// is treated as absent, and a present value excludes rows with no
    /// upstream status (transport failure, cache hit).
    #[serde(default)]
    pub last_upstream_status: Option<String>,
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
    input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_est: Option<f64>,
    /// Whether the response was served from the in-process response cache.
    cache_hit: bool,
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
    /// `service:outcome[:upstreamStatus]` per completed attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt_outcomes: Option<String>,
    /// Upstream status of the last attempt that reported one.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_upstream_status: Option<i64>,
    /// Distinct attempted key ids, comma-joined.
    #[serde(skip_serializing_if = "Option::is_none")]
    key_ids: Option<String>,
    /// `service:transition:keyId` per key-state transition.
    #[serde(rename = "keyTransitions", skip_serializing_if = "Option::is_none")]
    key_transitions_csv: Option<String>,
}

pub async fn list_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppQuery(q): AppQuery<ListLogsQuery>,
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
        error_kind: q.error_kind,
        last_upstream_status: q.last_upstream_status.and_then(|s| s.parse::<i64>().ok()),
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
        input_tokens: f.input_tokens,
        output_tokens: f.output_tokens,
        total_tokens: f.total_tokens,
        cost_est: f.cost_est,
        cache_hit: f.cache_hit,
        attempt_outcomes: f.attempt_outcomes,
        last_upstream_status: f.last_upstream_status,
        key_ids: f.key_ids,
        key_transitions_csv: f.key_transitions_csv,
        node_id: f.node_id,
    }
}
