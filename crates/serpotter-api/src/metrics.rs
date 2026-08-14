//! Prometheus metrics surface (B5): request counters by service/status class,
//! a request-duration histogram, a concurrent-in-flight gauge, cron-updated
//! key-pool depth per service, and an exact-query cache hit/miss counter.
//!
//! `observe` is called by `events::emit` for every product request (search /
//! extract / research / MCP tools / failed auth).
//! The in-flight gauge is maintained by [`metrics_middleware`]; the key-pool
//! depth gauge is refreshed by the maintenance cron each tick.
//!
//! Metric handles live in one dedicated [`prometheus::Registry`] (not the
//! process-global default) so the exposition is exactly this module's surface
//! and tests can reset counters deterministically.
//!
//! Wire-up (Main, per the Wave 3A route-registration rule):
//! ```ignore
//! .route("/metrics", get(metrics::scrape_metrics))
//! .layer(axum::middleware::from_fn(metrics::metrics_middleware))
//! ```

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
}

static METRICS: LazyLock<Metrics> = LazyLock::new(|| {
    let registry = Registry::new();
    let requests_total = IntCounterVec::new(
        Opts::new(
            "serpotter_requests_total",
            "Product requests written to request_log, by service and status class (ok|error).",
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

    Metrics {
        registry,
        requests_total,
        request_duration,
        requests_in_flight,
        key_pool_depth,
        cache_requests_total,
    }
});

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
/// `input_tokens` / `output_tokens` are carried for the next wave's token
/// metrics (no token gauge exists yet); `cache_hit` feeds the cache counter —
/// B1's serve signal is wire-only this wave, so callers pass `false` until the
/// product cache lands the hit flag on `ExecMeta`.
pub fn observe(
    status: i64,
    service: Option<&str>,
    duration: Duration,
    _input_tokens: Option<i64>,
    _output_tokens: Option<i64>,
    cache_hit: bool,
) {
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

/// In-flight bracket for the whole router: incremented before the inner stack
/// runs and decremented after, so the gauge returns to 0 between requests.
/// Wire as the OUTERMOST layer so it brackets the request-id/trace layers:
/// `.layer(axum::middleware::from_fn(metrics::metrics_middleware))`.
pub async fn metrics_middleware(req: Request<Body>, next: Next) -> Response<Body> {
    METRICS.requests_in_flight.inc();
    let res = next.run(req).await;
    METRICS.requests_in_flight.dec();
    res
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

        observe(
            200,
            Some("tavily"),
            Duration::from_millis(250),
            None,
            None,
            false,
        );
        observe(
            500,
            Some("tavily"),
            Duration::from_millis(250),
            None,
            None,
            true,
        );
        observe(200, None, Duration::from_millis(250), None, None, false);

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
    fn exposition_encodes_all_families() {
        let _guard = METRICS_LOCK.lock();
        METRICS.requests_total.reset();
        observe(
            200,
            Some("exa"),
            Duration::from_millis(10),
            None,
            None,
            false,
        );
        // A gauge family with zero children emits no TYPE line — seed one so
        // the exposition covers every family.
        METRICS.key_pool_depth.with_label_values(&["xai"]).set(1);
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
        assert!(text.contains(r#"serpotter_requests_total{service="exa",status_class="ok"} 1"#));
    }

    #[tokio::test]
    async fn middleware_brackets_in_flight_gauge() {
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
