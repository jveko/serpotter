//! Admin API: session tokens (argon2) and ADMIN_SECRET bootstrap.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use password_hash::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serpotter_auth::{
    authentication_error, generate_session_token, generate_token, problem_response,
};

use crate::AppState;

pub type AdminState = AppState;

/// Session TTL: 7 days (sqlite datetime offset).
const SESSION_TTL_DAYS: i64 = 7;

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let auth = headers.get(axum::http::header::AUTHORIZATION)?;
    let s = auth.to_str().ok()?;
    let rest = s.strip_prefix("Bearer ")?;
    let t = rest.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

fn verify_password(password: &str, password_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(password_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Auth order: valid unexpired session Bearer → ADMIN_SECRET Bearer → X-Admin-Password.
/// Session authorizes even when ADMIN_SECRET is unset.
async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    if let Some(token) = bearer_token(headers) {
        match state.db.get_valid_admin_session(&token).await {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(_) => {
                return Err(problem_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "DatabaseError",
                    "session lookup failed",
                ));
            }
        }
        // Fall through: may be ADMIN_SECRET as Bearer
        if let Some(secret) = state.admin_secret.as_deref().filter(|s| !s.is_empty()) {
            if token == secret {
                return Ok(());
            }
        }
    }

    if let Some(pw) = headers.get("x-admin-password") {
        if let Ok(s) = pw.to_str() {
            if let Some(secret) = state.admin_secret.as_deref().filter(|s| !s.is_empty()) {
                if s.trim() == secret {
                    return Ok(());
                }
            }
        }
    }

    // Distinguish disabled vs bad creds only when neither secret nor any session path worked
    // and ADMIN_SECRET is missing (and no session matched above).
    if state.admin_secret.as_deref().filter(|s| !s.is_empty()).is_none()
        && bearer_token(headers).is_none()
        && headers.get("x-admin-password").is_none()
    {
        return Err(problem_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "AdminDisabled",
            "ADMIN_SECRET not configured",
        ));
    }

    Err(authentication_error("Invalid admin credentials"))
}

fn admin_secret_matches(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(secret) = state.admin_secret.as_deref().filter(|s| !s.is_empty()) else {
        return false;
    };
    if let Some(token) = bearer_token(headers) {
        if token == secret {
            return true;
        }
    }
    if let Some(pw) = headers.get("x-admin-password") {
        if let Ok(s) = pw.to_str() {
            if s.trim() == secret {
                return true;
            }
        }
    }
    false
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapBody {
    pub username: Option<String>,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginOut {
    token: String,
    expires_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapOut {
    username: String,
    id: i64,
}

/// POST /api/admin/bootstrap — only when no admin_users and ADMIN_SECRET matches.
pub async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BootstrapBody>,
) -> impl IntoResponse {
    if !admin_secret_matches(&state, &headers) {
        if state.admin_secret.as_deref().filter(|s| !s.is_empty()).is_none() {
            return problem_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "AdminDisabled",
                "ADMIN_SECRET not configured",
            );
        }
        return authentication_error("Invalid admin credentials");
    }
    match state.db.count_admin_users().await {
        Ok(0) => {}
        Ok(_) => {
            return problem_response(
                StatusCode::CONFLICT,
                "AlreadyBootstrapped",
                "admin user already exists",
            );
        }
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    }
    let password = body.password.trim();
    if password.is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "password is required",
        );
    }
    let username = body
        .username
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("admin");
    let hash = match hash_password(password) {
        Ok(h) => h,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "HashError",
                e,
            );
        }
    };
    match state.db.insert_admin_user(username, &hash).await {
        Ok(user) => (
            StatusCode::CREATED,
            Json(BootstrapOut {
                username: user.username,
                id: user.id,
            }),
        )
            .into_response(),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// POST /api/admin/login — username/password → session token.
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginBody>,
) -> impl IntoResponse {
    let username = body.username.trim();
    let password = body.password.trim();
    if username.is_empty() || password.is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "username and password are required",
        );
    }
    let user = match state.db.get_admin_user_by_username(username).await {
        Ok(Some(u)) => u,
        Ok(None) => return authentication_error("Invalid credentials"),
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    if !verify_password(password, &user.password_hash) {
        return authentication_error("Invalid credentials");
    }
    let token = match generate_session_token() {
        Ok(t) => t,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "TokenError",
                e.to_string(),
            );
        }
    };
    let expires_at = match state.db.datetime_now_plus_days(SESSION_TTL_DAYS).await {
        Ok(s) => s,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    match state
        .db
        .insert_admin_session(&token, user.id, &expires_at)
        .await
    {
        Ok(sess) => Json(LoginOut {
            token: sess.token,
            expires_at: sess.expires_at,
        })
        .into_response(),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}


/// POST /api/admin/logout — invalidate Bearer session (204 even if unknown).
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = bearer_token(&headers) {
        let _ = state.db.delete_admin_session(&token).await;
    }
    StatusCode::NO_CONTENT
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

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncCreditsBody {
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncKeyResult {
    id: i64,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SyncCreditsOut {
    service: String,
    synced: i64,
    errors: i64,
    results: Vec<SyncKeyResult>,
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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

pub async fn list_nodes(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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
    if let Err(r) = require_admin(&state, &headers).await {
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

/// Soft-fail credit sync for tavily and/or firecrawl. Never sets active=0 on fetch fail.
pub async fn sync_credits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SyncCreditsBody>,
) -> impl IntoResponse {
    if let Err(r) = require_admin(&state, &headers).await {
        return r;
    }

    let services: Vec<&str> = match body.service.as_deref() {
        Some("tavily") => vec!["tavily"],
        Some("firecrawl") => vec!["firecrawl"],
        Some(other) => {
            return problem_response(
                StatusCode::BAD_REQUEST,
                "ValidationError",
                format!("unsupported service {other}"),
            );
        }
        None => vec!["tavily", "firecrawl"],
    };

    match crate::credit_sync::sync_credits_for_services(
        &state.db,
        &state.providers,
        &services,
    )
    .await
    {
        Ok(report) => (
            StatusCode::OK,
            Json(SyncCreditsOut {
                service: report.service,
                synced: report.synced,
                errors: report.errors,
                results: report
                    .results
                    .into_iter()
                    .map(|r| SyncKeyResult {
                        id: r.id,
                        ok: r.ok,
                        remaining: r.remaining,
                        limit: r.limit,
                    })
                    .collect(),
            }),
        )
            .into_response(),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
