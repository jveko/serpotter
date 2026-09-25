//! Admin API: session tokens (argon2) and ADMIN_SECRET bootstrap.

mod keys;
mod logs;
mod nodes;
mod session;
mod settings;
mod stats;
mod tokens;
mod usage;
use axum::http::{HeaderMap, StatusCode};
use serpotter_auth::{authentication_error, problem_response};
use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;

// Handler fns re-exported so route registration in `lib.rs` stays readable.
// Body/query DTOs stay private to their handler modules (never referenced
// through these re-exports).
pub use keys::{create_key, delete_key, list_keys, sync_credits, toggle_key, update_key};
pub use logs::list_request_logs;
pub use nodes::{create_node, delete_node, list_nodes, test_node, toggle_node, update_node};
pub use session::{
    bootstrap, change_password, list_sessions, login, logout, new_failure_store, revoke_session,
    FailureStore, FailureWindow,
};
pub use settings::{get_settings, put_settings};
pub use stats::stats;
pub use tokens::{create_token, delete_token, list_tokens};
pub use usage::{spend_by_keys, spend_by_services, usage};

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
    let value = auth.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Compare credentials without exiting early on the first differing byte.
/// Lengths are not secret in this protocol and are folded into the result.
fn constant_time_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(left.get(index).copied().unwrap_or_default())
            ^ usize::from(right.get(index).copied().unwrap_or_default());
    }
    difference == 0
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
            if constant_time_eq(secret, &token) {
                return Ok(());
            }
        }
    }

    if let Some(pw) = headers.get("x-admin-password") {
        if let Ok(supplied) = pw.to_str() {
            if let Some(secret) = ctx.admin_secret.as_deref().filter(|s| !s.is_empty()) {
                if constant_time_eq(secret, supplied.trim()) {
                    return Ok(());
                }
            }
        }
    }

    // Distinguish disabled vs bad creds only when neither secret nor any session path worked
    // and ADMIN_SECRET is missing (and no session matched above).
    if ctx
        .admin_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .is_none()
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
        if constant_time_eq(secret, &token) {
            return true;
        }
    }
    if let Some(pw) = headers.get("x-admin-password") {
        if let Ok(supplied) = pw.to_str() {
            if constant_time_eq(secret, supplied.trim()) {
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
    // Char-safe slicing: `&key[..4]` would panic on a multi-byte boundary.
    // For ASCII (the common case) this is byte-for-byte the previous output.
    let chars: Vec<char> = key.chars().collect();
    let head_end = chars.len().min(4);
    let tail_start = chars.len().saturating_sub(4);
    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();
    format!("{head}…{tail}")
}

pub(crate) fn mask_token(token: &str) -> String {
    if token.len() <= 12 {
        return "tok-****".into();
    }
    let chars: Vec<char> = token.chars().collect();
    let head_end = chars.len().min(8);
    let tail_start = chars.len().saturating_sub(4);
    let head: String = chars[..head_end].iter().collect();
    let tail: String = chars[tail_start..].iter().collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_scheme_is_case_insensitive() {
        for scheme in ["Bearer", "bearer", "BEARER", "BeArEr"] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("{scheme} token-value")).unwrap(),
            );
            assert_eq!(bearer_token(&headers).as_deref(), Some("token-value"));
        }
    }

    #[test]
    fn constant_time_compare_matches_equality() {
        assert!(constant_time_eq("secret", "secret"));
        assert!(!constant_time_eq("secret", "Secret"));
        assert!(!constant_time_eq("secret", "secre"));
        assert!(!constant_time_eq("", "x"));
    }
}
