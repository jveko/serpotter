//! Serpotter HTTP API: search, extract, research, MCP, admin.

mod admin;
mod credit_sync;
pub mod cron;
pub mod events;
mod mcp;
mod metrics;
mod product;
pub mod trace_layer;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_db::{Db, EXPECTED_SCHEMA_VERSION};
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_product::ProductCtx;
use serpotter_providers::ProviderRegistry;

pub use admin::{new_failure_store, AdminCtx, FailureStore, FailureWindow};
pub use mcp::{MCP_SESSION_HEADER, MCP_SESSION_TTL_SECS};
pub use serpotter_product::{
    ExtractRequest, ExtractResponse, ResearchRequest, ResearchResponse, SearchExecError,
};

/// Product settings parsed once when the application state is built.
/// Repeated requests reuse these values and never re-read or re-warn.
#[derive(Clone, Copy, Debug)]
pub struct ProductConfig {
    request_timeout: Duration,
    cache: CacheConfig,
}

impl ProductConfig {
    /// Parse product environment once, at application startup.
    pub fn from_env() -> Self {
        Self {
            request_timeout: product::request_timeout_from_env(),
            cache: parse_cache_ttl(std::env::var("CACHE_TTL_SECS").ok().as_deref()),
        }
    }

    /// Same config with a different overall request deadline (tests that need
    /// a short deadline without touching the process environment).
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }
}

impl Default for ProductConfig {
    /// The compiled defaults, independent of the environment. Used by test
    /// fixtures so a suite never inherits an ambient or mutated env value.
    fn default() -> Self {
        Self {
            request_timeout: product::DEFAULT_REQUEST_TIMEOUT,
            cache: CacheConfig::default(),
        }
    }
}

const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

/// Upper bound for `CACHE_TTL_SECS` (24 h). Mirrors
/// `MAX_REQUEST_TIMEOUT_SECS` in the product module: an absurd TTL is a
/// misconfiguration, not a longer cache.
const MAX_CACHE_TTL_SECS: u64 = 86_400;

/// Parse `CACHE_TTL_SECS`, warning once for any set-but-invalid value.
/// `0` disables the cache; `1..=MAX_CACHE_TTL_SECS` sets the TTL; anything
/// else (empty, non-numeric, negative, over the ceiling) falls back to
/// [`DEFAULT_CACHE_TTL`] with a warning.
fn parse_cache_ttl(value: Option<&str>) -> CacheConfig {
    let Some(raw) = value else {
        return CacheConfig::default();
    };

    let parsed = raw.trim().parse::<u64>();
    match parsed {
        Ok(0) => CacheConfig {
            enabled: false,
            ttl: DEFAULT_CACHE_TTL,
        },
        Ok(secs) if secs <= MAX_CACHE_TTL_SECS => CacheConfig {
            enabled: true,
            ttl: Duration::from_secs(secs),
        },
        _ => {
            tracing::warn!(
                value = %raw.trim(),
                default_secs = DEFAULT_CACHE_TTL.as_secs(),
                max_secs = MAX_CACHE_TTL_SECS,
                "invalid CACHE_TTL_SECS; using default"
            );
            CacheConfig::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CacheConfig {
    enabled: bool,
    ttl: Duration,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl: DEFAULT_CACHE_TTL,
        }
    }
}
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub outbound: Arc<ProxyPool>,
    pub providers: ProviderRegistry,
    /// Optional bootstrap admin secret (ADMIN_SECRET env).
    pub admin_secret: Option<String>,
    pub events: events::RequestEvents,
    /// Parsed once at construction and reused by every product request.
    pub product_config: ProductConfig,
    /// Admin login throttle bookkeeping, shared by all handlers in this
    /// process. Tests build a fresh store per AppState, so throttle state
    /// never leaks between test functions.
    pub login_failures: Arc<FailureStore>,
}

impl AppState {
    pub fn product_ctx(&self) -> ProductCtx {
        ProductCtx {
            db: self.db.clone(),
            keys: self.keys.clone(),
            outbound: self.outbound.clone(),
            providers: self.providers.clone(),
            progress: None,
            request_timeout: self.product_config.request_timeout,
            // F10 attribution: a fresh per-request sink; the deadline wrapper
            // clones the handle it reads after the product future is dropped.
            meta_sink: None,
            // B1: exact-query TTL cache. CACHE_TTL_SECS=0 disables; default 300.
            cache_enabled: self.product_config.cache.enabled,
            cache_ttl: self.product_config.cache.ttl,
        }
    }

    pub fn admin_ctx(&self) -> AdminCtx {
        AdminCtx {
            db: self.db.clone(),
            providers: self.providers.clone(),
            admin_secret: self.admin_secret.clone(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveBody {
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyBody {
    status: &'static str,
    schema_version: Option<i64>,
    expected: i64,
}

/// Explicit inbound body limit (2 MiB). Matches typical axum default; set deliberately.
pub const BODY_LIMIT_BYTES: usize = 2 * 1024 * 1024;

/// Build the router, reading the SPA directory from `ADMIN_SPA_DIR`.
pub fn app(state: AppState) -> Router {
    let spa_dir = std::env::var("ADMIN_SPA_DIR").ok();
    app_with_spa(state, spa_dir.as_deref())
}

/// Same as [`app`], with the SPA directory passed explicitly instead of read
/// from the environment. Lets tests exercise SPA routing without touching
/// process-global env state.
pub fn app_with_spa(state: AppState, spa_dir: Option<&str>) -> Router {
    let mut router = Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/api/search", post(product::search::search))
        .route("/api/extract", post(product::extract::extract_handler))
        .route("/api/research", post(product::extract::research_handler))
        // KNOWN GAP (deliberate, T-adminevents): a method-rejected (405) request
        // to one of these three still emits NO event. Every other boundary
        // rejection does — body rejections via `product::AppJsonLogged` (400 /
        // 422 / 415 / 413) and auth failures via `events::ApiTokenLogged` —
        // because both run inside the handler's extractor chain.
        //
        // Deferred as SCOPE, not as an architectural impossibility: axum
        // answers 405 inside the `MethodRouter`, so no extractor sees it, but a
        // response-inspecting layer (the same shape as the `from_fn` layers
        // added below, which DO observe 405s for the in-flight gauge) could
        // close it cheaply. The one hazard is double-emitting for a status a
        // handler already emits, which a path+status whitelist avoids. Left
        // alone here because it is a route/layer-shape change outside this
        // wave's event-funnel scope; until then a 405 leaves no ring row, no
        // error-window bucket and no metric.
        .nest_service("/mcp", mcp::service(state.clone()))
        // Admin
        .route("/api/admin/bootstrap", post(admin::bootstrap))
        .route("/api/admin/login", post(admin::login))
        .route("/api/admin/logout", post(admin::logout))
        .route(
            "/api/tokens",
            get(admin::list_tokens).post(admin::create_token),
        )
        .route("/api/tokens/{id}", delete(admin::delete_token))
        .route("/api/keys", get(admin::list_keys).post(admin::create_key))
        .route(
            "/api/keys/{id}",
            delete(admin::delete_key).put(admin::update_key),
        )
        .route("/api/keys/{id}/toggle", post(admin::toggle_key))
        .route("/api/keys/sync-credits", post(admin::sync_credits))
        .route(
            "/api/settings",
            get(admin::get_settings).put(admin::put_settings),
        )
        .route("/api/stats", get(admin::stats))
        .route("/api/request-logs", get(admin::list_request_logs))
        .route(
            "/api/nodes",
            get(admin::list_nodes).post(admin::create_node),
        )
        .route(
            "/api/nodes/{id}",
            delete(admin::delete_node).put(admin::update_node),
        )
        .route("/api/nodes/{id}/toggle", post(admin::toggle_node))
        .route("/api/nodes/{id}/test", post(admin::test_node))
        // B6: usage analytics + spend
        .route("/api/usage", get(admin::usage))
        .route("/api/spend/keys", get(admin::spend_by_keys))
        .route("/api/spend/services", get(admin::spend_by_services))
        // B14: admin password rotation + session revocation
        .route("/api/admin/change-password", post(admin::change_password))
        .route("/api/admin/sessions", get(admin::list_sessions))
        .route("/api/admin/sessions/{id}", delete(admin::revoke_session))
        // B5: prometheus exposition behind admin auth
        .route("/metrics", get(metrics::scrape_metrics))
        // Unknown /api paths answer a JSON problem, never the SPA's index.html.
        // Without this the root SPA fallback below would serve HTML with 200 to
        // a mistyped endpoint, which is far harder to debug than a 404.
        .route("/api", any(api_not_found))
        .route("/api/{*rest}", any(api_not_found))
        .with_state(state);

    // Optional static SPA at the site root: ADMIN_SPA_DIR=/path/to/web/dist.
    // ServeDir resolves real files (/assets/*, /favicon.ico); anything it cannot
    // find falls back to index.html, so refreshing a client route (/keys, /logs)
    // boots the app instead of 404ing. Registered as the router's fallback, so
    // every route declared above — /api, /mcp, /live, /ready — still wins.
    if let Some(dir) = spa_dir.map(str::trim).filter(|d| !d.is_empty()) {
        let index = std::path::Path::new(dir).join("index.html");
        let spa = tower_http::services::ServeDir::new(dir)
            .fallback(tower_http::services::ServeFile::new(index));
        router = router.fallback_service(spa);
    }

    // Request-id + trace stack, applied after the SPA fallback so every
    // response (API routes and SPA static files) carries a bounded
    // x-request-id. Layer order, last added = outermost (axum wraps each new
    // layer around the previous): `metrics_middleware` (outermost) ->
    // `bound_request_id` -> SetRequestIdLayer -> TraceLayer ->
    // PropagateRequestIdLayer (innermost). The bound middleware truncates an
    // oversized inbound x-request-id to MAX_REQUEST_ID_LEN bytes *before* the
    // set/trace/propagate layers see it, so spans, request_log rows, and the
    // propagated response header all observe the bounded id. Wired here
    // (inside `app_with_spa`) so the production stack and the integration-test
    // stack are identical; `main.rs` adds no layers of its own.
    let (set_request_id, trace, propagate) = trace_layer::build_http_layers();
    router
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .layer(propagate)
        .layer(trace)
        .layer(set_request_id)
        .layer(axum::middleware::from_fn(trace_layer::bound_request_id))
        // B5: in-flight gauge bracket, the OUTERMOST layer (last `.layer`
        // call) so it frames the request-id/trace stack. `Router::layer`
        // rewrites every already-registered endpoint — path routes AND the
        // catch-all fallback set by `fallback_service` above — so applying it
        // here also counts SPA/static traffic. Kept last on purpose: adding
        // any `.layer` after it would put that layer outside the bracket.
        .layer(axum::middleware::from_fn(metrics::metrics_middleware))
}

async fn api_not_found() -> axum::response::Response {
    problem_response(StatusCode::NOT_FOUND, "NotFound", "Unknown API endpoint")
}

async fn live() -> Json<LiveBody> {
    Json(LiveBody { status: "ok" })
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let expected = EXPECTED_SCHEMA_VERSION;
    match state.db.schema_version().await {
        Ok(version) if version >= expected => (
            StatusCode::OK,
            Json(ReadyBody {
                status: "ready",
                schema_version: Some(version),
                expected,
            }),
        )
            .into_response(),
        Ok(version) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyBody {
                status: "not_ready",
                schema_version: Some(version),
                expected,
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyBody {
                status: "not_ready",
                schema_version: None,
                expected,
            }),
        )
            .into_response(),
    }
}

/// Require a valid API token (Bearer or x-api-key). Returns the token row on success.
#[allow(clippy::result_large_err)]
pub async fn require_api_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<serpotter_db::TokenRow, axum::response::Response> {
    let Some(token) = extract_token(headers) else {
        return Err(authentication_error("Missing API token"));
    };
    match state.db.get_token_by_value(&token).await {
        Ok(Some(row)) => Ok(row),
        Ok(None) => Err(authentication_error("Invalid token")),
        Err(_) => Err(problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DatabaseError",
            "Token lookup failed",
        )),
    }
}

/// Shared test-only helpers for this crate's unit tests (lib target).
///
/// `cron.rs` and `main.rs` keep their own locks and sinks: they mutate a
/// disjoint set of variables (`KEY_REENABLE_AFTER_HOURS`, `ADMIN_ALERT_URL`,
/// `PORT`) from the product settings handled here. Integration tests under
/// `tests/` run in their own processes.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use super::events;
    use super::{new_failure_store, AppState, ProductConfig};

    /// Serializes process-env mutation across every unit test in the lib
    /// target, so parallel tests never race set/remove.
    pub(crate) static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Test-only capture sink for WARN+ events (Arc-owned buffer, no leak).
    #[derive(Clone, Default)]
    struct CaptureSink(Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Run `f` under a WARN-only subscriber, returning its value and the
    /// captured log text.
    pub(crate) fn capture_warns<T>(f: impl FnOnce() -> T) -> (T, String) {
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false) // CI runners emit ANSI escapes; assertions need plain text
            .with_writer(move || writer.clone())
            .finish();
        let value = tracing::subscriber::with_default(subscriber, f);
        let guard = sink.0.lock();
        (value, String::from_utf8_lossy(&guard).into_owned())
    }

    /// Minimal [`AppState`] for unit tests that need the real `ProductCtx`
    /// shape (e.g. the deadline wrapper, which reads/writes
    /// `ProductCtx::meta_sink`).
    ///
    /// It DOES touch a database — an in-memory one, migrated on the spot —
    /// because `ProductCtx` owns a `Db` and a key pool; there is no
    /// cheaper honest construction. Uses the compiled
    /// [`ProductConfig`] defaults, so it never reads the process environment.
    ///
    /// `RequestEvents::new` spawns the usage-writer task and returns its
    /// `JoinHandle`, which is dropped here: the task then idles on a channel
    /// no test writes to and is reaped when the test binary exits. Tests that
    /// assert usage rollup must drive `shutdown()` and await the handle
    /// themselves rather than use this helper.
    pub(crate) async fn app_state() -> AppState {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate in-memory test db");
        AppState {
            keys: Arc::new(serpotter_keypool::KeyPool::new(db.clone())),
            outbound: Arc::new(serpotter_outbound::ProxyPool::new(db.clone())),
            providers: serpotter_providers::ProviderRegistry::with_clients(
                serpotter_providers::TavilyClient::new("http://127.0.0.1:9"),
                serpotter_providers::FirecrawlClient::new("http://127.0.0.1:9"),
                serpotter_providers::ExaClient::new("http://127.0.0.1:9"),
                serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
            ),
            events: events::RequestEvents::new(db.clone()).0,
            db,
            admin_secret: None,
            product_config: ProductConfig::default(),
            login_failures: new_failure_store(),
        }
    }
}

/// Test-only read of the request counter for one `(service, status_class)`
/// label pair. Integration tests use it to prove an event reached the
/// METRICS side of the funnel, not just the ring.
#[doc(hidden)]
pub fn metrics_requests_count(service: &str, class: &str) -> u64 {
    metrics::test_requests_count(service, class)
}

/// Test-only read of the per-attempt counter for one `(service, outcome)`
/// label pair. Integration tests use it to prove a classified vendor failure
/// reached the METRICS side of the funnel even when the request's own row is
/// an error or a fallback answer.
#[doc(hidden)]
pub fn metrics_attempt_count(service: &str, outcome: &str) -> u64 {
    metrics::test_attempt_count(service, outcome)
}

/// Test-only read of how many requests the in-flight-gauge middleware has
/// bracketed. Integration tests use it to prove a route (notably the SPA
/// fallback and static assets) passes through the bracket — the gauge itself
/// reads 0 again once the response is done, so it cannot show that.
#[doc(hidden)]
pub fn metrics_middleware_hits() -> u64 {
    metrics::test_middleware_hits()
}

#[cfg(test)]
mod config_tests {
    use super::test_support::{capture_warns, ENV_LOCK};
    use super::*;
    use serpotter_providers::{ExaClient, FirecrawlClient, TavilyClient, XaiClient};

    struct EnvGuard {
        name: &'static str,
        original: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let original = std::env::var_os(name);
            std::env::set_var(name, value);
            Self { name, original }
        }

        fn set_ttl(value: &str) -> Self {
            Self::set("CACHE_TTL_SECS", value)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }
    #[test]
    fn cache_ttl_normalizes_unset_empty_invalid_zero_valid_and_over_ceiling_values() {
        let cases = [
            // unset: compiled default, no warning.
            (None, true, Duration::from_secs(300), false),
            (Some(""), true, Duration::from_secs(300), true),
            (Some("later"), true, Duration::from_secs(300), true),
            (Some("-1"), true, Duration::from_secs(300), true),
            (Some("0"), false, Duration::from_secs(300), false),
            (Some(" 45 "), true, Duration::from_secs(45), false),
            // Ceiling: the bound itself is accepted, anything above warns.
            (Some("86400"), true, Duration::from_secs(86_400), false),
            (Some("86401"), true, Duration::from_secs(300), true),
            (
                Some("18446744073709551615"),
                true,
                Duration::from_secs(300),
                true,
            ),
        ];

        for (value, enabled, ttl, should_warn) in cases {
            let (config, text) = capture_warns(|| parse_cache_ttl(value));
            assert_eq!(config.enabled, enabled, "enabled for {value:?}");
            assert_eq!(config.ttl, ttl, "ttl for {value:?}");
            if should_warn {
                assert!(
                    text.contains("invalid CACHE_TTL_SECS; using default")
                        && text.contains("default_secs=300")
                        && text.contains("max_secs=86400"),
                    "invalid cache value must name the default and range: {text}"
                );
                // An empty override renders as an empty value field, so only
                // non-empty inputs can be checked for the raw value itself.
                if let Some(raw) = value.filter(|raw| !raw.trim().is_empty()) {
                    assert!(
                        text.contains(&format!("value={}", raw.trim())),
                        "warn must carry the raw offending value: {text}"
                    );
                }
            } else {
                assert!(
                    text.is_empty(),
                    "valid cache value must not warn: {value:?}: {text}"
                );
            }
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // ENV_LOCK deliberately serializes env mutation across the test
    async fn product_config_warns_once_at_state_construction_and_product_ctx_reuses_it() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let _env_guard = ENV_LOCK.lock();
        // Both vars are pinned to invalid values BEFORE construction, so the
        // stored config never depends on the ambient environment.
        let _cache_invalid = EnvGuard::set_ttl("invalid-cache");
        let _timeout_invalid = EnvGuard::set("REQUEST_TIMEOUT_SECS", "invalid-timeout");
        let providers = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );

        let (state, initial_log) = capture_warns(|| AppState {
            db: db.clone(),
            keys: Arc::new(KeyPool::with_config(
                db.clone(),
                3,
                Duration::from_secs(30),
                serpotter_db::KEY_HOLD_TTL_SECS,
                serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
            )),
            outbound: Arc::new(ProxyPool::with_options_and_hold_ttl(
                db.clone(),
                false,
                serpotter_db::NODE_HOLD_TTL_SECS,
            )),
            providers,
            admin_secret: None,
            events: events::RequestEvents::new(db.clone()).0,
            product_config: ProductConfig::from_env(),
            login_failures: new_failure_store(),
        });

        assert_eq!(
            initial_log.matches("invalid CACHE_TTL_SECS").count(),
            1,
            "{initial_log}"
        );
        assert_eq!(
            initial_log.matches("invalid REQUEST_TIMEOUT_SECS").count(),
            1,
            "{initial_log}"
        );

        let _cache_valid = EnvGuard::set_ttl("45");
        let _timeout_valid = EnvGuard::set("REQUEST_TIMEOUT_SECS", "90");
        let (_, repeated_log) = capture_warns(|| {
            for _ in 0..3 {
                let ctx = state.product_ctx();
                assert!(ctx.cache_enabled);
                assert_eq!(ctx.cache_ttl, Duration::from_secs(300));
                assert_eq!(ctx.request_timeout, Duration::from_secs(120));
            }
        });
        assert!(
            repeated_log.is_empty(),
            "product_ctx must reuse state without warnings: {repeated_log}"
        );
    }
}
