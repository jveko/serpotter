//! HTTP tracing layer with a request-id-aware MakeSpan.
//!
//! Layer order (last = outermost), per tower-http docs:
//! `PropagateRequestIdLayer` (inner) → `make_trace_layer()` → `SetRequestIdLayer` (outer).
//! The propagate layer copies an inbound `x-request-id` into the request extensions
//! before the trace span is created; the set layer mints the UUID only for requests
//! that arrived without one. MakeSpan therefore reads the extension and never mints
//! a second ID.

use axum::http::{Request, Response};
use std::time::Duration;
use tower_http::classify::{ServerErrorsAsFailures, SharedClassifier};
use tower_http::request_id::RequestId;
use tower_http::trace::TraceLayer;
use tracing::Span;

fn make_span<B>(request: &Request<B>) -> Span {
    // Prefer the extension set by PropagateRequestIdLayer; fall back to the
    // inbound header when the propagate layer is absent (e.g. unit tests).
    // Never mint a second UUID here — SetRequestIdLayer owns generation.
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            request
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        });

    // method + path only — the full URI is noise and can embed user data.
    match request_id {
        Some(request_id) => tracing::info_span!(
            "http.request",
            method = %request.method(),
            path = request.uri().path(),
            request_id = %request_id,
        ),
        None => tracing::info_span!(
            "http.request",
            method = %request.method(),
            path = request.uri().path(),
        ),
    }
}

fn on_request<B>(_request: &Request<B>, span: &Span) {
    tracing::info!(parent: span, "request started");
}

fn on_response<B>(response: &Response<B>, latency: Duration, span: &Span) {
    tracing::info!(
        parent: span,
        status = response.status().as_u16(),
        latency_ms = latency.as_millis() as u64,
        "request finished",
    );
}

/// `TraceLayer` with a `MakeSpan` that records `method`, `path`, and
/// `request_id` (from the `RequestId` extension, inbound-header fallback).
/// OnRequest/OnResponse events log at INFO; headers are never included.
pub fn make_trace_layer<B>() -> TraceLayer<
    SharedClassifier<ServerErrorsAsFailures>,
    fn(&Request<B>) -> Span,
    fn(&Request<B>, &Span),
    fn(&Response<B>, Duration, &Span),
> {
    TraceLayer::new_for_http()
        .make_span_with(make_span::<B> as fn(&Request<B>) -> Span)
        .on_request(on_request::<B> as fn(&Request<B>, &Span))
        .on_response(on_response::<B> as fn(&Response<B>, Duration, &Span))
}
