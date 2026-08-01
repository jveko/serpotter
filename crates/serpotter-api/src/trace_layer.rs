//! HTTP tracing layer with a request-id-aware MakeSpan.
//!
//! Layer order (last = outermost), per tower-http docs:
//! `PropagateRequestIdLayer` (inner) → `make_trace_layer()` → `SetRequestIdLayer` (outer).
//! The set layer stores the *effective* request id in the `RequestId` request
//! extension — an inbound `x-request-id` header wins, otherwise it mints a UUID.
//! The propagate layer copies that extension onto the response `x-request-id`
//! header. MakeSpan therefore reads the extension and never mints a second ID.

use axum::http::{Request, Response};
use std::time::Duration;
use tower_http::classify::{ServerErrorsAsFailures, SharedClassifier};
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;
use tracing::Span;

fn make_span<B>(request: &Request<B>) -> Span {
    // Read the effective id from the RequestId extension, which SetRequestIdLayer
    // populated (inbound header wins, else minted UUID). Fall back to the raw
    // inbound header only when the set layer is absent (e.g. unit tests).
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

/// `TraceLayer` shape used by Serpotter: server-error classification plus the
/// request/response hooks installed in [`make_trace_layer`]. Factored into a
/// type alias so the fn signature stays readable.
pub type HttpTraceLayer<B> = TraceLayer<
    SharedClassifier<ServerErrorsAsFailures>,
    fn(&Request<B>) -> Span,
    fn(&Request<B>, &Span),
    fn(&Response<B>, Duration, &Span),
>;

/// `TraceLayer` with a `MakeSpan` that records `method`, `path`, and
/// `request_id` (from the `RequestId` extension, inbound-header fallback).
/// OnRequest/OnResponse events log at INFO; headers are never included.
pub fn make_trace_layer<B>() -> HttpTraceLayer<B> {
    TraceLayer::new_for_http()
        .make_span_with(make_span::<B> as fn(&Request<B>) -> Span)
        .on_request(on_request::<B> as fn(&Request<B>, &Span))
        .on_response(on_response::<B> as fn(&Response<B>, Duration, &Span))
}

/// The full request-id + trace layer stack in serve order: `(set, trace,
/// propagate)`, outer first. Mirrors the main.rs wiring so tests exercise the
/// same assembly; apply with `.layer(propagate).layer(trace).layer(set)`.
pub fn build_http_layers<B>() -> (
    SetRequestIdLayer<MakeRequestUuid>,
    HttpTraceLayer<B>,
    PropagateRequestIdLayer,
) {
    (
        SetRequestIdLayer::x_request_id(MakeRequestUuid),
        make_trace_layer(),
        PropagateRequestIdLayer::x_request_id(),
    )
}
