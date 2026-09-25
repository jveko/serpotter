//! Admin bootstrap, login, logout.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use password_hash::rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serpotter_auth::{authentication_error, generate_session_token, problem_response};

use super::{admin_secret_matches, bearer_token, mask_token, require_admin, SESSION_TTL_DAYS};
use crate::AppState;

const LOGIN_FAILURE_LIMIT: usize = 10;
const LOGIN_FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);
const MAX_TRACKED_CLIENTS: usize = 4096;
const UNKNOWN_CLIENT: &str = "unknown";
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$1TlUO3Xyr/swLudLkEfLPg$PXi0sW+yVdSGUrRM5CLirKcm3lFxYfXF1bu1NPMsmro";

/// Per-client failed-authentication bookkeeping backing the admin throttle.
/// One instance lives in [`crate::AppState`]: production shares a single
/// store across handlers, while every test AppState gets a fresh one.
#[derive(Debug, Default)]
pub struct FailureWindow {
    failures: HashMap<String, VecDeque<Instant>>,
    insertion_order: VecDeque<String>,
}

impl FailureWindow {
    fn prune(&mut self, identity: &str, now: Instant) {
        let Some(attempts) = self.failures.get_mut(identity) else {
            return;
        };
        while attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= LOGIN_FAILURE_WINDOW)
        {
            attempts.pop_front();
        }
        if attempts.is_empty() {
            self.failures.remove(identity);
            self.insertion_order
                .retain(|candidate| candidate != identity);
        }
    }

    fn is_blocked(&mut self, identity: &str, now: Instant) -> bool {
        self.prune(identity, now);
        self.failures
            .get(identity)
            .is_some_and(|attempts| attempts.len() >= LOGIN_FAILURE_LIMIT)
    }

    /// Drop every entry whose window has fully lapsed. Self-healing: without
    /// this sweep a store saturated with expired lockouts could never free a
    /// slot and would silently stop tracking new clients. O(n) over at most
    /// `MAX_TRACKED_CLIENTS` entries, on the failure path only (logins that
    /// fail are rare), so the cost is not on any success path.
    fn sweep_expired(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .failures
            .iter()
            .filter(|(_, attempts)| {
                attempts
                    .back()
                    .is_none_or(|last| now.duration_since(*last) >= LOGIN_FAILURE_WINDOW)
            })
            .map(|(identity, _)| identity.clone())
            .collect();
        for identity in &expired {
            self.failures.remove(identity);
        }
        if !expired.is_empty() {
            self.insertion_order
                .retain(|candidate| !expired.contains(candidate));
        }
    }

    /// Evict bookkeeping for the oldest tracked client that is safe to drop:
    /// one that is below the limit, or whose failures have all aged out. A
    /// live lockout is never evicted, so a flood of distinct addresses cannot
    /// release another client's block.
    fn evict_if_full(&mut self, incoming: &str, now: Instant) -> bool {
        if self.failures.len() < MAX_TRACKED_CLIENTS || self.failures.contains_key(incoming) {
            return true;
        }
        self.sweep_expired(now);
        if self.failures.len() < MAX_TRACKED_CLIENTS {
            return true;
        }
        let victim = self.insertion_order.iter().find(|identity| {
            self.failures
                .get(identity.as_str())
                .is_some_and(|attempts| {
                    attempts.len() < LOGIN_FAILURE_LIMIT
                        || attempts
                            .back()
                            .is_some_and(|last| now.duration_since(*last) >= LOGIN_FAILURE_WINDOW)
                })
        });
        match victim.cloned() {
            Some(victim) => {
                self.failures.remove(&victim);
                self.insertion_order
                    .retain(|candidate| candidate != &victim);
                true
            }
            // Every tracked client is locked out right now: drop the new
            // record rather than release somebody else's lockout.
            None => false,
        }
    }

    fn record_failure(&mut self, identity: &str, now: Instant) {
        self.prune(identity, now);
        if !self.evict_if_full(identity, now) {
            return;
        }
        let attempts = self.failures.entry(identity.to_string()).or_default();
        if attempts.is_empty() {
            self.insertion_order.push_back(identity.to_string());
        }
        attempts.push_back(now);
    }

    /// Drop one client's failures (called after a successful authentication).
    fn forget(&mut self, identity: &str) {
        self.failures.remove(identity);
        self.insertion_order
            .retain(|candidate| candidate != identity);
    }

    #[cfg(test)]
    pub(crate) fn tracked_clients(&self) -> usize {
        self.failures.len()
    }

    #[cfg(test)]
    pub(crate) fn tracked_order(&self) -> usize {
        self.insertion_order.len()
    }
}

/// Shared lock for one [`FailureWindow`].
///
/// A poisoned lock means a handler panicked while holding it. Throttle access
/// then degrades to a no-op with a warning rather than recovering possibly
/// inconsistent state or propagating a second panic into request handling.
pub type FailureStore = Mutex<FailureWindow>;

/// Build the throttle store an [`crate::AppState`] holds.
pub fn new_failure_store() -> Arc<FailureStore> {
    Arc::new(Mutex::new(FailureWindow::default()))
}

fn client_identity(
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
) -> String {
    connect_info
        .map(|Extension(connect_info)| connect_info.0.ip().to_string())
        .unwrap_or_else(|| UNKNOWN_CLIENT.to_string())
}

fn login_blocked(store: &FailureStore, identity: &str, now: Instant) -> bool {
    match store.lock() {
        Ok(mut window) => window.is_blocked(identity, now),
        Err(_) => {
            tracing::warn!("admin login throttle lock poisoned; treating client as unblocked");
            false
        }
    }
}

fn record_login_failure(store: &FailureStore, identity: &str, now: Instant) {
    match store.lock() {
        Ok(mut window) => window.record_failure(identity, now),
        Err(_) => tracing::warn!("admin login throttle lock poisoned; failure not recorded"),
    }
}

fn clear_login_failures(store: &FailureStore, identity: &str) {
    match store.lock() {
        Ok(mut window) => window.forget(identity),
        Err(_) => tracing::warn!("admin login throttle lock poisoned; failures not cleared"),
    }
}

fn too_many_attempts() -> axum::response::Response {
    problem_response(
        StatusCode::TOO_MANY_REQUESTS,
        "TooManyRequests",
        "too many failed admin authentication attempts; try again later",
    )
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
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<BootstrapBody>,
) -> impl IntoResponse {
    let identity = client_identity(connect_info);
    if login_blocked(&state.login_failures, &identity, Instant::now()) {
        return too_many_attempts();
    }
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
        record_login_failure(&state.login_failures, &identity, Instant::now());
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
    if password.len() < 8 {
        return problem_response(
            StatusCode::BAD_REQUEST,
            "ValidationError",
            "password must be at least 8 characters",
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
        Ok(user) => {
            clear_login_failures(&state.login_failures, &identity);
            (
                StatusCode::CREATED,
                Json(BootstrapOut {
                    username: user.username,
                    id: user.id,
                }),
            )
                .into_response()
        }
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
    connect_info: Option<Extension<axum::extract::ConnectInfo<SocketAddr>>>,
    Json(body): Json<LoginBody>,
) -> impl IntoResponse {
    let identity = client_identity(connect_info);
    if login_blocked(&state.login_failures, &identity, Instant::now()) {
        return too_many_attempts();
    }
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
        Ok(None) => {
            let _ = verify_password(password, DUMMY_PASSWORD_HASH);
            record_login_failure(&state.login_failures, &identity, Instant::now());
            return authentication_error("Invalid credentials");
        }
        Err(e) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    };
    if !verify_password(password, &user.password_hash) {
        record_login_failure(&state.login_failures, &identity, Instant::now());
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
        Ok(sess) => {
            clear_login_failures(&state.login_failures, &identity);
            Json(LoginOut {
                token: sess.token,
                expires_at: sess.expires_at,
            })
            .into_response()
        }
        Err(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

/// POST /api/admin/logout — invalidate Bearer session. Unknown or already
/// expired sessions are idempotent 204 responses; database failures are 500.
pub async fn logout(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Some(token) = bearer_token(&headers) {
        if let Err(e) = ctx.db.delete_admin_session(&token).await {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                e.to_string(),
            );
        }
    }
    StatusCode::NO_CONTENT.into_response()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_window_blocks_at_limit_and_recovers() {
        let mut window = FailureWindow::default();
        let start = Instant::now();
        for _ in 0..LOGIN_FAILURE_LIMIT {
            assert!(!window.is_blocked("client", start));
            window.record_failure("client", start);
        }
        assert!(window.is_blocked("client", start));
        assert!(!window.is_blocked("other", start));
        assert!(!window.is_blocked("client", start + LOGIN_FAILURE_WINDOW));
    }

    #[test]
    fn lockouts_survive_a_flood_of_distinct_clients() {
        let mut window = FailureWindow::default();
        let start = Instant::now();
        for _ in 0..LOGIN_FAILURE_LIMIT {
            window.record_failure("victim", start);
        }
        for index in 0..MAX_TRACKED_CLIENTS * 2 {
            window.record_failure(&format!("flood-{index}"), start);
        }
        assert!(window.is_blocked("victim", start));
        assert!(window.tracked_clients() <= MAX_TRACKED_CLIENTS);
        assert!(window.tracked_order() <= MAX_TRACKED_CLIENTS);
    }

    #[test]
    fn live_lockouts_refuse_new_identities() {
        let mut window = FailureWindow::default();
        let start = Instant::now();
        for client in 0..MAX_TRACKED_CLIENTS {
            let identity = format!("locked-{client}");
            for _ in 0..LOGIN_FAILURE_LIMIT {
                window.record_failure(&identity, start);
            }
        }
        window.record_failure("fresh", start);
        assert!(!window.is_blocked("fresh", start));
        assert_eq!(window.tracked_clients(), MAX_TRACKED_CLIENTS);
    }

    #[test]
    fn saturated_store_with_expired_lockouts_tracks_new_clients_again() {
        let mut window = FailureWindow::default();
        let start = Instant::now();
        for client in 0..MAX_TRACKED_CLIENTS {
            let identity = format!("locked-{client}");
            for _ in 0..LOGIN_FAILURE_LIMIT {
                window.record_failure(&identity, start);
            }
        }
        assert_eq!(window.tracked_clients(), MAX_TRACKED_CLIENTS);

        // Every stored lockout has aged out: the sweep must free the whole
        // store instead of leaving it permanently saturated and untracked.
        let after_window = start + LOGIN_FAILURE_WINDOW;
        for index in 0..10 {
            window.record_failure(&format!("post-{index}"), after_window);
        }
        assert_eq!(window.tracked_clients(), 10);
        assert_eq!(window.tracked_order(), 10);
        assert!(!window.is_blocked("post-0", after_window));
    }

    #[test]
    fn expiry_removes_client_from_map_and_order() {
        let mut window = FailureWindow::default();
        let start = Instant::now();
        window.record_failure("client", start);
        assert_eq!(window.tracked_order(), 1);
        window.record_failure("client", start + LOGIN_FAILURE_WINDOW);
        assert_eq!(window.tracked_order(), 1, "one live entry only");
        assert!(!window.is_blocked("client", start + LOGIN_FAILURE_WINDOW * 2));
        assert_eq!(window.tracked_clients(), 0);
        assert_eq!(window.tracked_order(), 0);
    }

    #[test]
    fn successful_login_does_not_duplicate_the_order_entry() {
        let mut window = FailureWindow::default();
        let start = Instant::now();
        window.record_failure("client", start);
        let after_window = start + LOGIN_FAILURE_WINDOW;
        window.record_failure("client", after_window);
        assert!(!window.is_blocked("client", after_window));
        assert_eq!(window.tracked_order(), 1);
        assert_eq!(window.tracked_clients(), 1);
    }

    #[test]
    fn dummy_hash_matches_argon2_default_parameters() {
        let parsed = PasswordHash::new(DUMMY_PASSWORD_HASH).expect("valid dummy PHC hash");
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.params.get_str("m"), Some("19456"));
        assert_eq!(parsed.params.get_str("t"), Some("2"));
        assert_eq!(parsed.params.get_str("p"), Some("1"));
    }
}
