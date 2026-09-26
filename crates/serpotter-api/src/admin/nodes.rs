//! Proxy nodes admin handlers.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;

use super::extract::database_problem;
use super::extract::{bounded_credentials, bounded_field, valid_node_host, AppJson, AppPath};
use super::require_admin;
use crate::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeOut {
    id: i64,
    host: String,
    port: i64,
    protocol: String,
    enabled: bool,
    inflight: i64,
    consecutive_fails: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_until: Option<String>,
}

fn node_out(r: serpotter_db::NodeRow) -> NodeOut {
    NodeOut {
        id: r.id,
        host: r.host,
        port: r.port,
        protocol: r.protocol,
        enabled: r.enabled != 0,
        inflight: r.inflight,
        consecutive_fails: r.consecutive_fails,
        username: r.username,
        last_error: r.last_error,
        lease_until: r.lease_until,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNodeBody {
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub protocol: Option<String>,
}

pub async fn list_nodes(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.list_nodes().await {
        Ok(rows) => {
            let out: Vec<NodeOut> = rows.into_iter().map(node_out).collect();
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}

pub async fn create_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppJson(body): AppJson<CreateNodeBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    if body.port < 1 || body.port > 65535 {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "host and valid port (1–65535) required",
        );
    }
    // `host` is interpolated raw into the proxy URL, so a scheme/port/path/
    // credential there would repoint the proxy instead of failing at create.
    let host = match bounded_field("host", &body.host) {
        Ok(h) if !h.is_empty() => h,
        Ok(_) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                "host and valid port (1–65535) required",
            );
        }
        Err(detail) => {
            return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
        }
    };
    if !valid_node_host(host) {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            format!("host {host:?} is not a valid host: use a DNS name, IPv4, or [IPv6] literal without a scheme, port, path, or credentials"),
        );
    }
    let protocol = body
        .protocol
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("http");
    if !serpotter_db::is_allowed_node_protocol(protocol) {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "protocol must be http, https, or socks5",
        );
    }
    let username = match bounded_field("username", body.username.as_deref().unwrap_or("")) {
        Ok(u) => (!u.is_empty()).then_some(u),
        Err(detail) => {
            return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
        }
    };
    let password = match bounded_field("password", body.password.as_deref().unwrap_or("")) {
        Ok(p) => (!p.is_empty()).then_some(p),
        Err(detail) => {
            return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
        }
    };
    match ctx
        .db
        .insert_node(host, body.port, username, password, protocol)
        .await
    {
        Ok(row) => {
            let out = node_out(row);
            (StatusCode::CREATED, Json(out)).into_response()
        }
        Err(e) => database_problem(e),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNodeBody {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<i64>,
    #[serde(default)]
    pub protocol: Option<String>,
    /// Absent = keep; explicit `null` = clear stored credential; string = set.
    /// `double_option` distinguishes an explicit JSON `null` (→ `Some(None)`)
    /// from a missing field (→ `None`), which plain `Option<Option<T>>` cannot.
    #[serde(default, deserialize_with = "double_option")]
    pub username: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub password: Option<Option<String>>,
}

/// Serde helper: JSON `null` → `Ok(Some(None))`, a value → `Ok(Some(Some(v)))`,
/// and (with `#[serde(default)]`) a missing field stays `None`.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// Patch a node's connection settings. `{host?, port?, protocol?, username?, password?}`
/// — at least one field required; missing fields keep their current value and an
/// explicit `null` username/password clears the stored credential.
pub async fn update_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
    AppJson(body): AppJson<UpdateNodeBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }

    let host = match body.host.as_deref() {
        None => None,
        Some(raw) => match bounded_field("host", raw) {
            Ok(h) if !h.is_empty() => Some(h),
            Ok(_) => {
                return problem_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationError",
                    "host must not be blank",
                );
            }
            Err(detail) => {
                return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail);
            }
        },
    };
    if let Some(h) = host {
        if !valid_node_host(h) {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                format!("host {h:?} is not a valid host: use a DNS name, IPv4, or [IPv6] literal without a scheme, port, path, or credentials"),
            );
        }
    }
    if body.port.is_some_and(|p| !(1..=65535).contains(&p)) {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "port must be in 1–65535",
        );
    }
    let protocol = body.protocol.as_deref().map(str::trim);
    if let Some(p) = protocol {
        if !serpotter_db::is_allowed_node_protocol(p) {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                "protocol must be http, https, or socks5",
            );
        }
    }
    if body.host.is_none()
        && body.port.is_none()
        && body.protocol.is_none()
        && body.username.is_none()
        && body.password.is_none()
    {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "at least one field required",
        );
    }

    let username = match bounded_credentials("username", body.username.as_ref()) {
        Ok(v) => v,
        Err(detail) => return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail),
    };
    let password = match bounded_credentials("password", body.password.as_ref()) {
        Ok(v) => v,
        Err(detail) => return problem_response(StatusCode::BAD_REQUEST, "ValidationError", detail),
    };
    match ctx
        .db
        .update_node(id, host, body.port, protocol, username, password)
        .await
    {
        Ok(Some(updated)) => (StatusCode::OK, Json(node_out(updated))).into_response(),
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
        Err(e) => database_problem(e),
    }
}

pub async fn delete_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.delete_node(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
        Err(e) => database_problem(e),
    }
}

pub async fn toggle_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.get_node(id).await {
        Ok(Some(row)) => {
            let next = row.enabled == 0;
            match ctx.db.set_node_enabled(id, next).await {
                Ok(true) => match ctx.db.get_node(id).await {
                    Ok(Some(updated)) => {
                        let out = node_out(updated);
                        (StatusCode::OK, Json(out)).into_response()
                    }
                    Ok(None) => {
                        problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found")
                    }
                    Err(e) => database_problem(e),
                },
                Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
                Err(e) => database_problem(e),
            }
        }
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
        Err(e) => database_problem(e),
    }
}

/// Result of a live connectivity probe (`POST /api/nodes/{id}/test`). Always
/// 200 when the node exists: `ok:false` + `error` on failure (SPA-friendly),
/// `ok:true` + `latencyMs` on success.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeTestOut {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Probe one node's connectivity through the outbound pool's own proxy URL
/// builder (B12). The probe never touches node health state — it is a pure
/// read-only admin check.
pub async fn test_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    AppPath(id): AppPath<i64>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    match ctx.db.get_node(id).await {
        Ok(Some(row)) => {
            let out =
                match serpotter_outbound::test_node(&row, serpotter_outbound::NODE_PROBE_TIMEOUT)
                    .await
                {
                    Ok(latency) => NodeTestOut {
                        ok: true,
                        latency_ms: Some(latency.as_millis() as u64),
                        error: None,
                    },
                    Err(e) => NodeTestOut {
                        ok: false,
                        latency_ms: None,
                        error: Some(e),
                    },
                };
            (StatusCode::OK, Json(out)).into_response()
        }
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
        Err(e) => database_problem(e),
    }
}
