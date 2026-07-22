//! Admin bootstrap, login, logout.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use password_hash::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serpotter_auth::{
    authentication_error, generate_session_token, problem_response,
};

use super::{admin_secret_matches, bearer_token, SESSION_TTL_DAYS};
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
            return problem_response(StatusCode::INTERNAL_SERVER_ERROR, "HashError", e);
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
