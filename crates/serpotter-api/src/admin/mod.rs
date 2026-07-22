//! Admin API: session tokens (argon2) and ADMIN_SECRET bootstrap.

mod keys;
mod nodes;
mod session;
mod settings;
mod stats;
mod tokens;

use axum::http::{HeaderMap, StatusCode};
use serpotter_auth::{authentication_error, problem_response};
use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;

use crate::AppState;

// Body DTOs re-exported so public handler signatures stay reachable from `lib` routes.
#[allow(unused_imports)]
pub use keys::{
    create_key, delete_key, list_keys, sync_credits, toggle_key, CreateKeyBody, SyncCreditsBody,
};
#[allow(unused_imports)]
pub use nodes::{create_node, delete_node, list_nodes, CreateNodeBody};
#[allow(unused_imports)]
pub use session::{bootstrap, login, logout, BootstrapBody, LoginBody};
#[allow(unused_imports)]
pub use settings::{get_settings, put_settings, SettingsIn};
pub use stats::stats;
#[allow(unused_imports)]
pub use tokens::{create_token, delete_token, list_tokens, CreateTokenBody};

/// Alias retained for external callers that typed admin state as AppState.
pub type AdminState = AppState;

/// Admin domain context (db + providers for credit sync + bootstrap secret).
#[derive(Clone)]
pub struct AdminCtx {
    pub db: Db,
    pub providers: ProviderRegistry,
    pub admin_secret: Option<String>,
}

/// Session TTL: 7 days (sqlite datetime offset).
pub(crate) const SESSION_TTL_DAYS: i64 = 7;

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<String> {
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

/// Auth order: valid unexpired session Bearer → ADMIN_SECRET Bearer → X-Admin-Password.
/// Session authorizes even when ADMIN_SECRET is unset.
pub(crate) async fn require_admin(
    ctx: &AdminCtx,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    if let Some(token) = bearer_token(headers) {
        match ctx.db.get_valid_admin_session(&token).await {
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
        if let Some(secret) = ctx.admin_secret.as_deref().filter(|s| !s.is_empty()) {
            if token == secret {
                return Ok(());
            }
        }
    }

    if let Some(pw) = headers.get("x-admin-password") {
        if let Ok(s) = pw.to_str() {
            if let Some(secret) = ctx.admin_secret.as_deref().filter(|s| !s.is_empty()) {
                if s.trim() == secret {
                    return Ok(());
                }
            }
        }
    }

    // Distinguish disabled vs bad creds only when neither secret nor any session path worked
    // and ADMIN_SECRET is missing (and no session matched above).
    if ctx.admin_secret.as_deref().filter(|s| !s.is_empty()).is_none()
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

pub(crate) fn admin_secret_matches(ctx: &AdminCtx, headers: &HeaderMap) -> bool {
    let Some(secret) = ctx.admin_secret.as_deref().filter(|s| !s.is_empty()) else {
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

pub(crate) fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".into();
    }
    format!("{}…{}", &key[..4], &key[key.len() - 4..])
}

pub(crate) fn mask_token(token: &str) -> String {
    if token.len() <= 12 {
        return "tok-****".into();
    }
    format!("{}…{}", &token[..8], &token[token.len() - 4..])
}
