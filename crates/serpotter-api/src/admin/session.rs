//! Admin bootstrap, login, logout.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use password_hash::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serpotter_auth::{authentication_error, generate_session_token, problem_response};

use super::{admin_secret_matches, bearer_token, mask_token, require_admin, SESSION_TTL_DAYS};
use crate::AppState;

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

/// POST /api/admin/bootstrap — only when no admin_users and ADMIN_SECRET matches.
pub async fn bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BootstrapBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if !admin_secret_matches(&ctx, &headers) {
        if ctx
            .admin_secret
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_none()
        {
            return problem_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "AdminDisabled",
                "ADMIN_SECRET not configured",
            );
        }
        return authentication_error("Invalid admin credentials");
    }
    match ctx.db.count_admin_users().await {
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
            return problem_response(StatusCode::INTERNAL_SERVER_ERROR, "HashError", e);
        }
    };
    match ctx.db.insert_admin_user(username, &hash).await {
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
    let ctx = state.admin_ctx();
    let username = body.username.trim();
    let password = body.password.trim();
    if username.is_empty() || password.is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "username and password are required",
        );
    }
    let user = match ctx.db.get_admin_user_by_username(username).await {
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
    let expires_at = match ctx.db.datetime_now_plus_days(SESSION_TTL_DAYS).await {
        Ok(s) => s,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    match ctx
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
    let ctx = state.admin_ctx();
    if let Some(token) = bearer_token(&headers) {
        let _ = ctx.db.delete_admin_session(&token).await;
    }
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePasswordBody {
    pub current_password: String,
    pub new_password: String,
}

/// POST /api/admin/change-password — verify the current password, store the new
/// hash, and revoke every OTHER session (the caller's session survives).
/// 401 wrong current password; 400 short/blank new password.
pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordBody>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let current = body.current_password.trim();
    let new = body.new_password.trim();
    if current.is_empty() {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "current password required",
        );
    }
    if new.len() < 8 {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "new password must be at least 8 characters",
        );
    }
    let users = match ctx.db.list_admin_users().await {
        Ok(users) => users,
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    let Some(user) = users
        .iter()
        .find(|u| verify_password(current, &u.password_hash))
    else {
        return authentication_error("Invalid current password");
    };
    if verify_password(new, &user.password_hash) {
        // New must differ from current (defensive; argon2 verify on the same string).
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "new password must differ from current",
        );
    }
    let hash = match hash_password(new) {
        Ok(h) => h,
        Err(e) => {
            return problem_response(StatusCode::INTERNAL_SERVER_ERROR, "HashError", e);
        }
    };
    if let Err(e) = ctx.db.update_admin_password_hash(user.id, &hash).await {
        return problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        );
    }
    // Revoke other sessions. `keep` may be the ADMIN_SECRET (no session match)
    // or the caller's adm- session; either way the caller keeps working.
    let keep = bearer_token(&headers);
    if let Err(e) = ctx.db.revoke_admin_sessions_except(keep.as_deref()).await {
        return problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        );
    }
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionOut {
    /// Full session token — the only stable id (admin_sessions PK). The SPA
    /// masks it for display and revokes by this value.
    token: String,
    token_preview: String,
    user_id: i64,
    expires_at: String,
    created_at: String,
    current: bool,
}

/// GET /api/admin/sessions — list active sessions (no hashes), newest first.
/// `current` marks the caller's own bearer token when authz was a session.
pub async fn list_sessions(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let current_token = bearer_token(&headers);
    // Only mark current when the caller was authorized by a real session
    // (an ADMIN_SECRET bearer is not a session row).
    let current_is_session = match current_token.as_deref() {
        Some(t) => matches!(ctx.db.get_valid_admin_session(t).await, Ok(Some(_))),
        None => false,
    };
    match ctx.db.list_admin_sessions().await {
        Ok(rows) => {
            let out: Vec<SessionOut> = rows
                .into_iter()
                .map(|r| SessionOut {
                    token: r.token.clone(),
                    token_preview: mask_token(&r.token),
                    user_id: r.user_id,
                    expires_at: r.expires_at,
                    created_at: r.created_at,
                    current: current_is_session
                        && current_token.as_deref() == Some(r.token.as_str()),
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

/// DELETE /api/admin/sessions/{id} — revoke one session by token. 404 unknown.
pub async fn revoke_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let token = id.trim();
    if token.is_empty() {
        return problem_response(StatusCode::NOT_FOUND, "NotFound", "session not found");
    }
    match ctx.db.revoke_admin_session(token).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => problem_response(StatusCode::NOT_FOUND, "NotFound", "session not found"),
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}
