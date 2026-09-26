//! In-flight gauge bracket coverage (T-metrics).
//!
//! Its own test binary on purpose: the proof is a process-global hit counter,
//! and the requests must be attributable one-for-one to the URIs under test.
//! Sharing a binary with sibling SPA tests would make a concurrent test's
//! requests land in the same delta.
//!
//! The defect: `metrics_middleware` was applied to the router BEFORE
//! `fallback_service(spa)`. axum 0.8's `Router::layer` rewrites only the
//! endpoints registered so far (it maps `path_router`, `fallback_router` and
//! `catch_all_fallback` of the *current* router), so the SPA fallback added
//! afterwards bypassed the middleware entirely — the gauge silently ignored the
//! admin console and its static assets.

mod common;

use axum::body::Body;
use axum::response::Response;
use axum::Router;
use common::*;
use serpotter_api::app_with_spa;

/// Minimal built-SPA shape: index.html + a hashed-asset stand-in.
const SPA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/spa");

/// The hit counter is process-global, so these tests must not interleave. A
/// `tokio::sync` lock (not `std::sync`) so the loser yields instead of blocking
/// the single-threaded test runtime.
static BRACKET_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn get(app: &Router, uri: &str) -> Response {
    app.clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// Every request path that only the SPA fallback can serve is bracketed by the
/// in-flight gauge: the SPA shell, a client-route refresh, and a real static
/// asset. The hit counter is what proves it — the gauge itself reads 0 again by
/// the time the response completes, so it cannot distinguish "ran and balanced"
/// from "never ran for this route".
#[tokio::test]
async fn fallback_served_requests_are_bracketed() {
    let _guard = BRACKET_LOCK.lock().await;
    let app = app_with_spa(state_with(test_db().await), Some(SPA_DIR));

    for uri in ["/", "/keys", "/assets/app.js"] {
        let before = serpotter_api::metrics_middleware_hits();
        let res = get(&app, uri).await;
        assert_eq!(res.status(), StatusCode::OK, "GET {uri}");
        assert_eq!(
            serpotter_api::metrics_middleware_hits(),
            before + 1,
            "GET {uri} is only reachable through the SPA fallback, so it must \
             pass through the in-flight gauge"
        );
    }
}

/// The same bracket covers the declared routes, and no request escapes it:
/// after N requests the counter has advanced by exactly N, so the middleware
/// neither double-counts nor drops a route.
#[tokio::test]
async fn declared_routes_are_bracketed_exactly_once() {
    let _guard = BRACKET_LOCK.lock().await;
    let app = app_with_spa(state_with(test_db().await), Some(SPA_DIR));

    let uris = ["/live", "/ready", "/api/nope"];
    let before = serpotter_api::metrics_middleware_hits();
    for uri in uris {
        get(&app, uri).await;
    }
    assert_eq!(
        serpotter_api::metrics_middleware_hits(),
        before + uris.len() as u64,
        "every declared route passes through the bracket exactly once"
    );
}

/// With no SPA configured the bracket still covers the router's own 404 — the
/// fallback-free stack must not be the case where the gauge is skipped.
#[tokio::test]
async fn router_without_spa_fallback_is_still_bracketed() {
    let _guard = BRACKET_LOCK.lock().await;
    let app = app_with_spa(state_with(test_db().await), None);

    let before = serpotter_api::metrics_middleware_hits();
    let res = get(&app, "/").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        serpotter_api::metrics_middleware_hits(),
        before + 1,
        "the router's own not-found must be bracketed"
    );
}

/// `app()` builds the same stack through the env-var path (no `ADMIN_SPA_DIR`
/// in the test environment, so the router has no fallback service). Coverage
/// must not depend on a SPA being mounted.
#[tokio::test]
async fn env_driven_app_is_bracketed_without_a_spa() {
    let _guard = BRACKET_LOCK.lock().await;
    let app = app(state_with(test_db().await));

    let before = serpotter_api::metrics_middleware_hits();
    let res = get(&app, "/live").await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        serpotter_api::metrics_middleware_hits(),
        before + 1,
        "app() must wire the same outermost bracket"
    );
}
