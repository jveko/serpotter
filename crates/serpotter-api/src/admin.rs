//! Admin API gated by ADMIN_SECRET (Bearer or X-Admin-Password).

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serpotter_auth::{authentication_error, generate_token, problem_response};

use crate::AppState;

pub type AdminState = AppState;

#[allow(clippy::result_large_err)]
fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), axum::response::Response> {
    let Some(secret) = state.admin_secret.as_deref().filter(|s| !s.is_empty()) else {
        return Err(problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "AdminDisabled",
            "ADMIN_SECRET not configured",
        ));
    };

    // Authorization: Bearer <secret>
    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some(rest) = s.strip_prefix("Bearer ") {
                if rest.trim() == secret {
                    return Ok(());
                }
            }
        }
    }
    // X-Admin-Password: <secret>
    if let Some(pw) = headers.get("x-admin-password") {
        if let Ok(s) = pw.to_str() {
            if s.trim() == secret {
                return Ok(());
            }
        }
    }
    Err(authentication_error("Invalid admin credentials"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenOut {
    id: i64,
    name: String,
    /// Full token only on create; list masks middle.
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_preview: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTokenBody {
    #[serde(default)]
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KeyOut {
    id: i64,
    service: String,
    key_preview: String,
    active: bool,
    consecutive_fails: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateKeyBody {
    pub service: String,
    pub key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsOut {
    social_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsIn {
    #[serde(default)]
    pub social_enabled: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatsOut {
    tokens: i64,
    api_keys: i64,
    active_api_keys: i64,
    nodes: i64,
    schema_version: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeOut {
    id: i64,
    host: String,
    port: i64,
    enabled: bool,
    inflight: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNodeBody {
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".into();
    }
    format!("{}…{}", &key[..4], &key[key.len() - 4..])
}

fn mask_token(token: &str) -> String {
    if token.len() <= 12 {
        return "tok-****".into();
    }
    format!("{}…{}", &token[..8], &token[token.len() - 4..])
}

pub async fn list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.list_tokens().await {
        Ok(rows) => {
            let out: Vec<TokenOut> = rows
                .into_iter()
                .map(|r| TokenOut {
                    id: r.id,
                    name: r.name,
                    token: None,
                    token_preview: Some(mask_token(&r.token)),
                    created_at: r.created_at,
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

pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateTokenBody>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let token = match generate_token() {
        Ok(t) => t,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TokenError",
                e.to_string(),
            );
        }
    };
    match state.db.insert_token(&token, &body.name).await {
        Ok(row) => {
            let out = TokenOut {
                id: row.id,
                name: row.name,
                token: Some(token),
                token_preview: None,
                created_at: row.created_at,
            };
            (StatusCode::CREATED, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn delete_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.delete_token_by_id(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "token not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn list_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.list_api_keys().await {
        Ok(rows) => {
            let out: Vec<KeyOut> = rows
                .into_iter()
                .map(|r| KeyOut {
                    id: r.id,
                    service: r.service,
                    key_preview: mask_key(&r.key),
                    active: r.active != 0,
                    consecutive_fails: r.consecutive_fails,
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

pub async fn create_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateKeyBody>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    if body.service.trim().is_empty() || body.key.trim().is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "service and key required",
        );
    }
    match state
        .db
        .insert_api_key(body.service.trim(), body.key.trim())
        .await
    {
        Ok(row) => {
            let out = KeyOut {
                id: row.id,
                service: row.service,
                key_preview: mask_key(&row.key),
                active: row.active != 0,
                consecutive_fails: row.consecutive_fails,
            };
            (StatusCode::CREATED, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.delete_api_key(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn toggle_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.get_api_key(id).await {
        Ok(Some(row)) => {
            let next = row.active == 0;
            match state.db.set_api_key_active(id, next).await {
                Ok(true) => {
                    let out = KeyOut {
                        id: row.id,
                        service: row.service,
                        key_preview: mask_key(&row.key),
                        active: next,
                        consecutive_fails: if next { 0 } else { row.consecutive_fails },
                    };
                    (StatusCode::OK, Json(out)).into_response()
                }
                Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
                Err(e) => problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DatabaseError",
                    e.to_string(),
                ),
            }
        }
        Ok(None) => problem_response(StatusCode::NOT_FOUND, "NotFound", "key not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.get_social_enabled().await {
        Ok(social_enabled) => {
            let out = SettingsOut {
                social_enabled,
                note: None,
            };
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn put_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SettingsIn>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    if let Some(v) = body.social_enabled {
        if let Err(e) = state.db.set_social_enabled(v).await {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    }
    match state.db.get_social_enabled().await {
        Ok(social_enabled) => {
            let out = SettingsOut {
                social_enabled,
                note: None,
            };
            (StatusCode::OK, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn stats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let tokens = state.db.count_tokens().await.unwrap_or(0);
    let api_keys = state.db.count_api_keys().await.unwrap_or(0);
    let active_api_keys = state.db.count_active_api_keys().await.unwrap_or(0);
    let nodes = state.db.count_nodes().await.unwrap_or(0);
    let schema_version = state.db.schema_version().await.unwrap_or(0);
    let out = StatsOut {
        tokens,
        api_keys,
        active_api_keys,
        nodes,
        schema_version,
    };
    (StatusCode::OK, Json(out)).into_response()
}

pub async fn list_nodes(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.list_nodes().await {
        Ok(rows) => {
            let out: Vec<NodeOut> = rows
                .into_iter()
                .map(|r| NodeOut {
                    id: r.id,
                    host: r.host,
                    port: r.port,
                    enabled: r.enabled != 0,
                    inflight: r.inflight,
                    username: r.username,
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

pub async fn create_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateNodeBody>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    if body.host.trim().is_empty() || body.port <= 0 {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "host and positive port required",
        );
    }
    match state
        .db
        .insert_node(
            body.host.trim(),
            body.port,
            body.username.as_deref(),
            body.password.as_deref(),
        )
        .await
    {
        Ok(row) => {
            let out = NodeOut {
                id: row.id,
                host: row.host,
                port: row.port,
                enabled: row.enabled != 0,
                inflight: row.inflight,
                username: row.username,
            };
            (StatusCode::CREATED, Json(out)).into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub async fn delete_node(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    match state.db.delete_node(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "node not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
