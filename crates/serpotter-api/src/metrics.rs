//! Prometheus metrics surface (B5): request counters by service/status class,
//! a request-duration histogram, a concurrent-in-flight gauge, cron-updated
//! key-pool depth per service, and an exact-query cache hit/miss counter.
//!
//! `observe` is called by `events::emit` for every product request (search /
//! extract / research / MCP tools / failed auth). It records the request
//! counter, the duration histogram and the exact-query cache hit/miss
//! counter. Token counts and cost are NOT collected here: they are printed on
//! the per-request audit line and roll up into `usage_daily`; a second
//! per-request token series would only duplicate that data.
//!
//! The in-flight gauge is maintained by [`metrics_middleware`]; the key-pool
//! depth gauge is refreshed by the maintenance cron each tick.
//!
//! Metric handles live in one dedicated [`prometheus::Registry`] (not the
//! process-global default) so the exposition is exactly this module's surface
//! and tests can reset counters deterministically.
//!
//! Wire-up (per the Wave 3A route-registration rule), in `app_with_spa`:
//! ```ignore
//! .route("/metrics", get(metrics::scrape_metrics))
//! // ...fallback_service(spa)...
//! .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
//! .layer(axum::middleware::from_fn(metrics::metrics_middleware))
//! ```
//! Both layers sit AFTER the fallback so they also cover SPA/static traffic;
//! `metrics_middleware` is the last `.layer` call, hence the outermost layer
//! in the returned stack. See its doc comment for the axum ordering proof.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
};
use serpotter_auth::problem_response;
use serpotter_db::Db;

use crate::admin::require_admin;
use crate::AppState;

struct Metrics {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration: HistogramVec,
    requests_in_flight: IntGauge,
    key_pool_depth: IntGaugeVec,
    cache_requests_total: IntCounterVec,
    events_dropped_total: IntCounterVec,
    provider_attempt_total: IntCounterVec,
    key_transition_total: IntCounterVec,
}

static METRICS: LazyLock<Metrics> = LazyLock::new(|| {
    let registry = Registry::new();
    let requests_total = IntCounterVec::new(
        Opts::new(
            "serpotter_requests_total",
            "Product request events by service and status class (ok|error).",
        ),
        &["service", "status_class"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(requests_total.clone()))
        .expect("register");

    let request_duration = HistogramVec::new(
        HistogramOpts::new(
            "serpotter_request_duration_seconds",
            "Request duration in seconds, by service.",
        )
        .buckets(vec![
            0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0,
        ]),
        &["service"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(request_duration.clone()))
        .expect("register");

    let requests_in_flight = IntGauge::new(
        "serpotter_requests_in_flight",
        "HTTP requests currently being processed (gauge, middleware-maintained).",
    )
    .expect("metric def valid");
    registry
        .register(Box::new(requests_in_flight.clone()))
        .expect("register");

    let key_pool_depth = IntGaugeVec::new(
        Opts::new(
            "serpotter_key_pool_depth",
            "Active provider keys in the pool, by service (cron-updated; 0 when all keys are disabled).",
        ),
        &["service"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(key_pool_depth.clone()))
        .expect("register");

    let cache_requests_total = IntCounterVec::new(
        Opts::new(
            "serpotter_cache_requests_total",
            "Exact-query cache requests by outcome (hit|miss).",
        ),
        &["hit"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(cache_requests_total.clone()))
        .expect("register");

    let events_dropped_total = IntCounterVec::new(
        Opts::new(
            "serpotter_events_dropped_total",
            "Request events dropped by the usage writer, by reason (channel_full|upsert_failed).",
        ),
        &["reason"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(events_dropped_total.clone()))
        .expect("register");

    let provider_attempt_total = IntCounterVec::new(
        Opts::new(
            "serpotter_provider_attempt_total",
            "Upstream provider attempts by service and outcome (ok|payment_required|rate_limited|auth_invalid|forbidden|banned|retryable|failure).",
        ),
        &["service", "outcome"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(provider_attempt_total.clone()))
        .expect("register");

    let key_transition_total = IntCounterVec::new(
        Opts::new(
            "serpotter_key_transition_total",
            "Key-state transitions caused by provider health reports, by service and transition (disabled|credits_zeroed|suspended|deleted).",
        ),
        &["service", "transition"],
    )
    .expect("metric def valid");
    registry
        .register(Box::new(key_transition_total.clone()))
        .expect("register");

    Metrics {
        registry,
        requests_total,
        request_duration,
        requests_in_flight,
        provider_attempt_total,
        key_transition_total,
        key_pool_depth,
        cache_requests_total,
        events_dropped_total,
    }
});

/// Number of times [`metrics_middleware`] has run, for the SPA/static
/// coverage test. NOT a Prometheus family: the in-flight gauge is back at 0
/// once a request finishes, so without a monotonic hit count an ordering test
/// could not distinguish "the layer ran and balanced" from "the layer never ran
/// for this route at all".
static MIDDLEWARE_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 2xx answers are `ok`; everything else (401, 429, 499, 5xx, …) is `error`.
fn status_class(status: i64) -> &'static str {
    if (200..300).contains(&status) {
        "ok"
    } else {
        "error"
    }
}

/// Record one finished product request. Called by `events::emit` for every
/// product request.
///
/// `cache_hit` feeds the exact-query cache counter; the caller passes the
/// real flag from `ExecMeta` (a `false` value is a genuine cache miss).
/// Token counts and cost are deliberately not parameters: they are printed on
/// the per-request audit line and roll up into `usage_daily`, so a second
/// per-request token series here would only duplicate that data.
pub fn observe(status: i64, service: Option<&str>, duration: Duration, cache_hit: bool) {
    let svc = service.unwrap_or("unknown");
    METRICS
        .requests_total
        .with_label_values(&[svc, status_class(status)])
        .inc();
    METRICS
        .request_duration
        .with_label_values(&[svc])
        .observe(duration.as_secs_f64());
    METRICS
        .cache_requests_total
        .with_label_values(&[if cache_hit { "hit" } else { "miss" }])
        .inc();
}

/// Count one dropped usage delta (loud accounting: the audit line already
/// landed in the log stream, so a drop only undercounts a rollup cell).
pub fn record_drop(reason: &'static str) {
    METRICS
        .events_dropped_total
        .with_label_values(&[reason])
        .inc();
}

/// Count one upstream provider attempt. Called by `events::emit` for every
/// record the product appended to `ExecMeta::attempt_log`, so a classified
/// failure stays countable even when the request's own row reads as a
/// fallback or an error.
pub fn observe_attempt(service: &str, outcome: &str) {
    METRICS
        .provider_attempt_total
        .with_label_values(&[service, outcome])
        .inc();
}

/// Count one key-state transition. Called by `events::emit` for every record
/// the product appended to `ExecMeta::key_transitions`, so a key quietly
/// leaving the rotation is countable per request — the `active = 0` flip is
/// invisible in the request row and in `serpotter_key_pool_depth` until the
/// next cron tick.
pub fn observe_key_transition(service: &str, transition: &str) {
    METRICS
        .key_transition_total
        .with_label_values(&[service, transition])
        .inc();
}

/// Test-only read of the per-transition counter for one `(service, transition)`
/// label pair — the funnel proof that a key really left the rotation, without
/// exposing the registry.
#[doc(hidden)]
pub fn test_key_transition_count(service: &str, transition: &str) -> u64 {
    METRICS
        .key_transition_total
        .with_label_values(&[service, transition])
        .get()
}

/// Test-only read of the request counter for one `(service, status_class)`
/// label pair. Exists so a test can prove an event reached the METRICS side
/// of the funnel, not just the ring — without exposing the registry.
#[doc(hidden)]
pub fn test_requests_count(service: &str, class: &str) -> u64 {
    METRICS
        .requests_total
        .with_label_values(&[service, class])
        .get()
}

/// Test-only read of the per-attempt counter for one `(service, outcome)`
/// label pair — the funnel proof that a vendor-level failure reached the
/// metrics sink, without exposing the registry.
#[doc(hidden)]
pub fn test_attempt_count(service: &str, outcome: &str) -> u64 {
    METRICS
        .provider_attempt_total
        .with_label_values(&[service, outcome])
        .get()
}

/// Test-only read of how many requests [`metrics_middleware`] has bracketed.
/// Lets a test prove the layer covers a given route (SPA fallback, static
/// asset) rather than only that the gauge balanced to 0 afterwards — after the
/// response, the gauge reads 0 whether or not the layer ever ran.
#[doc(hidden)]
pub fn test_middleware_hits() -> u64 {
    MIDDLEWARE_HITS.load(std::sync::atomic::Ordering::Relaxed)
}

/// In-flight bracket for the whole router: incremented before the inner stack
/// runs and decremented after, so the gauge returns to 0 between requests.
///
/// `Router::layer` rewrites every already-registered endpoint — path routes,
/// the fallback router AND the catch-all fallback (axum 0.8.9
/// `Router::layer` / `Router::fallback_service`) — so this layer must be
/// applied AFTER `fallback_service(spa)` to count SPA/static traffic. In
/// `app_with_spa` it is the last `.layer` call, which makes it the outermost
/// layer: each `.layer` call wraps the router built so far, so the
/// request-id/trace layers — added just before it — end up INSIDE this
/// bracket, and any layer added after it would end up outside.
pub async fn metrics_middleware(req: Request<Body>, next: Next) -> Response<Body> {
    MIDDLEWARE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Drop-scoped decrement: a client disconnect mid-response drops the
    // request future, and a handler panic unwinds through it. Either way the
    // guard's destructor still balances the gauge, so a leaked `inc()` can
    // never leave `serpotter_requests_in_flight` permanently off by one.
    let _in_flight = InFlightGuard::new();
    next.run(req).await
}

/// ZST RAII decrement of [`METRICS`]'s in-flight gauge. Holds no state — the
/// decrement happens in `Drop` so it survives future cancellation.
struct InFlightGuard;

impl InFlightGuard {
    fn new() -> Self {
        METRICS.requests_in_flight.inc();
        Self
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        METRICS.requests_in_flight.dec();
    }
}

/// Cron hook: set `serpotter_key_pool_depth{service}` to the number of ACTIVE
/// keys per service. Every service that has any key row gets a label (value 0
/// when all of its keys are disabled); labels are reset first so a service
/// that emptied does not keep a stale positive gauge.
pub async fn refresh_key_pool_depth(db: &Db) {
    let keys = match db.list_api_keys().await {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "list_api_keys failed; key pool depth gauge not updated");
            return;
        }
    };
    let mut by_service: BTreeMap<&str, i64> = BTreeMap::new();
    for k in &keys {
        let depth = by_service.entry(k.service.as_str()).or_insert(0);
        if k.active != 0 {
            *depth += 1;
        }
    }
    METRICS.key_pool_depth.reset();
    for (svc, depth) in by_service {
        METRICS.key_pool_depth.with_label_values(&[svc]).set(depth);
    }
}

/// GET /metrics — Prometheus text exposition, behind admin auth
/// (valid session Bearer or ADMIN_SECRET, same gate as every /api admin
/// route). Content-Type is the standard exposition format.
pub async fn scrape_metrics(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let ctx = state.admin_ctx();
    if let Err(r) = require_admin(&ctx, &headers).await {
        return r;
    }
    let mut buf = Vec::new();
    let encoder = TextEncoder::new();
    if let Err(e) = encoder.encode(&METRICS.registry.gather(), &mut buf) {
        return problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "MetricsError",
            e.to_string(),
        );
    }
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        buf,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use std::future::Future;
    use tower::ServiceExt;

    /// Serializes access to the shared metric registry for reset-based tests.
    static METRICS_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn status_class_maps_2xx_to_ok_everything_else_error() {
        assert_eq!(status_class(200), "ok");
        assert_eq!(status_class(299), "ok");
        assert_eq!(status_class(199), "error");
        assert_eq!(status_class(300), "error");
        assert_eq!(status_class(401), "error");
        assert_eq!(status_class(429), "error");
        assert_eq!(status_class(499), "error");
        assert_eq!(status_class(500), "error");
    }

    #[test]
    fn observe_increments_counters_and_histogram() {
        let _guard = METRICS_LOCK.lock();
        METRICS.requests_total.reset();
        METRICS.request_duration.reset();
        METRICS.cache_requests_total.reset();

        observe(200, Some("tavily"), Duration::from_millis(250), false);
        observe(500, Some("tavily"), Duration::from_millis(250), true);
        observe(200, None, Duration::from_millis(250), false);

        assert_eq!(
            METRICS
                .requests_total
                .with_label_values(&["tavily", "ok"])
                .get(),
            1
        );
        assert_eq!(
            METRICS
                .requests_total
                .with_label_values(&["tavily", "error"])
                .get(),
            1
        );
        // Missing service is attributed to "unknown", never panics.
        assert_eq!(
            METRICS
                .requests_total
                .with_label_values(&["unknown", "ok"])
                .get(),
            1
        );
        assert_eq!(
            METRICS
                .cache_requests_total
                .with_label_values(&["hit"])
                .get(),
            1
        );
        assert_eq!(
            METRICS
                .cache_requests_total
                .with_label_values(&["miss"])
                .get(),
            2
        );
        let observed = METRICS
            .request_duration
            .with_label_values(&["tavily"])
            .get_sample_sum();
        assert!(
            (observed - 0.5).abs() < f64::EPSILON * 10.0,
            "two 250ms observations sum to 0.5s, got {observed}"
        );
    }

    #[test]
    fn record_drop_increments_reason_labeled_counter() {
        let _guard = METRICS_LOCK.lock();
        METRICS.events_dropped_total.reset();
        record_drop("channel_full");
        record_drop("channel_full");
        record_drop("upsert_failed");
        assert_eq!(
            METRICS
                .events_dropped_total
                .with_label_values(&["channel_full"])
                .get(),
            2
        );
        assert_eq!(
            METRICS
                .events_dropped_total
                .with_label_values(&["upsert_failed"])
                .get(),
            1
        );
    }

    #[test]
    fn exposition_encodes_all_families() {
        let _guard = METRICS_LOCK.lock();
        // Every counter this test asserts on must be reset, not just
        // `requests_total`: a sibling test that ran first and called
        // `record_drop("channel_full")` would otherwise leave the exposition
        // reading 2 (or 3) where this test asserts exactly 1.
        METRICS.requests_total.reset();
        METRICS.events_dropped_total.reset();
        METRICS.provider_attempt_total.reset();
        METRICS.key_transition_total.reset();
        observe(200, Some("exa"), Duration::from_millis(10), false);
        // A gauge family with zero children emits no TYPE line — seed one so
        // the exposition covers every family.
        METRICS.key_pool_depth.with_label_values(&["xai"]).set(1);
        record_drop("channel_full");
        observe_attempt("exa", "auth_invalid");
        observe_key_transition("exa", "disabled");
        let mut buf = Vec::new();
        TextEncoder::new()
            .encode(&METRICS.registry.gather(), &mut buf)
            .expect("encode");
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("# TYPE serpotter_requests_total counter"));
        assert!(text.contains("# TYPE serpotter_request_duration_seconds histogram"));
        assert!(text.contains("# TYPE serpotter_requests_in_flight gauge"));
        assert!(text.contains("# TYPE serpotter_key_pool_depth gauge"));
        assert!(text.contains("# TYPE serpotter_cache_requests_total counter"));
        assert!(text.contains("# TYPE serpotter_events_dropped_total counter"));
        assert!(text.contains("# TYPE serpotter_provider_attempt_total counter"));
        assert!(text.contains("# TYPE serpotter_key_transition_total counter"));
        assert!(text.contains(r#"serpotter_requests_total{service="exa",status_class="ok"} 1"#));
        assert!(text.contains(r#"serpotter_events_dropped_total{reason="channel_full"} 1"#));
        // Prometheus renders label pairs sorted by name, not declaration order.
        assert!(text.contains(
            r#"serpotter_provider_attempt_total{outcome="auth_invalid",service="exa"} 1"#
        ));
        assert!(text
            .contains(r#"serpotter_key_transition_total{service="exa",transition="disabled"} 1"#));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // METRICS_LOCK deliberately serializes the whole test
    async fn middleware_brackets_in_flight_gauge() {
        let _guard = METRICS_LOCK.lock();
        let app = Router::new()
            .route("/x", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(metrics_middleware));
        let res = app
            .oneshot(Request::builder().uri("/x").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            METRICS.requests_in_flight.get(),
            0,
            "gauge must return to zero after the request"
        );
    }

    /// A client that disconnects mid-response drops the request future, so a
    /// `dec()` after `next.run(req).await` would never run and the gauge would
    /// drift upward by one for the life of the process. The guard's `Drop`
    /// balances the gauge during unwinding instead.
    ///
    /// The future is polled once and then DROPPED, which is exactly what a
    /// cancelled request does to its future. It is deliberately not spawned:
    /// a spawned task runs on another thread, outside `METRICS_LOCK`, so its
    /// teardown could race a sibling test reading the same gauge.
    #[tokio::test]
    async fn cancelled_request_still_balances_in_flight_gauge() {
        let _guard = METRICS_LOCK.lock();
        let app = Router::new()
            .route(
                "/hang",
                get(|| async {
                    // Never completes: the request stays pending after the
                    // first poll, so only the drop can finish the future.
                    std::future::pending::<()>().await;
                    "unreachable"
                }),
            )
            .layer(axum::middleware::from_fn(metrics_middleware));

        let baseline = METRICS.requests_in_flight.get();
        let mut request =
            Box::pin(app.oneshot(Request::builder().uri("/hang").body(Body::empty()).unwrap()));
        // One poll runs the middleware (gauge +1) into the never-finishing
        // handler; the future is then dropped, unwinding the guard.
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(
            request.as_mut().poll(&mut cx).is_pending(),
            "the request must park in the handler, not complete"
        );
        assert_eq!(
            METRICS.requests_in_flight.get(),
            baseline + 1,
            "the bracket is held while the request is in flight"
        );
        drop(request);

        assert_eq!(
            METRICS.requests_in_flight.get(),
            baseline,
            "dropping a cancelled request future must balance the gauge"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // METRICS_LOCK deliberately serializes the whole test
    async fn refresh_key_pool_depth_counts_active_keys_per_service() {
        let _guard = METRICS_LOCK.lock();
        METRICS.key_pool_depth.reset();
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        db.insert_api_key("tavily", "tvly-depth-1").await.unwrap();
        db.insert_api_key("tavily", "tvly-depth-2").await.unwrap();
        db.insert_api_key("exa", "ek-depth-1").await.unwrap();
        let exa = db.insert_api_key("exa", "ek-depth-disabled").await.unwrap();
        db.set_api_key_active(exa.id, false).await.unwrap();

        refresh_key_pool_depth(&db).await;

        assert_eq!(
            METRICS.key_pool_depth.with_label_values(&["tavily"]).get(),
            2,
            "two active tavily keys"
        );
        assert_eq!(
            METRICS.key_pool_depth.with_label_values(&["exa"]).get(),
            1,
            "disabled key is not depth; label still present"
        );
        // A service with only disabled keys reports depth 0, not a stale value.
        METRICS.key_pool_depth.with_label_values(&["exa"]).set(99);
        refresh_key_pool_depth(&db).await;
        assert_eq!(
            METRICS.key_pool_depth.with_label_values(&["exa"]).get(),
            1,
            "refresh overwrites, never accumulates"
        );
    }
}
