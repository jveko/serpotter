//! HTTP tracing layer with a request-id-aware MakeSpan.
//!
//! Layer order (last = outermost), per tower-http docs:
//! `PropagateRequestIdLayer` (inner) → `make_trace_layer()` → `SetRequestIdLayer` (outer).
//! The set layer stores the *effective* request id in the `RequestId` request
//! extension — an inbound `x-request-id` header wins, otherwise it mints a
//! bounded hex id. The propagate layer copies that extension onto the response
//! `x-request-id` header. MakeSpan therefore reads the extension and never
//! mints a second ID.
//!
//! Inbound ids are bounded: a `x-request-id` longer than [`MAX_REQUEST_ID_LEN`]
//! bytes is truncated before it lands in the extension (and the request
//! header), so spans, request_log rows, and the response header all observe
//! the bounded value. [`bound_request_id`] runs outermost and pre-sets the
//! bounded extension; the set layer then skips its verbatim copy and the
//! minting maker only fires when no inbound header exists.

use axum::body::Body;
use axum::http::{HeaderValue, Request, Response};
use axum::middleware::Next;
use std::time::Duration;
use tower_http::classify::{ServerErrorsAsFailures, SharedClassifier};
use tower_http::request_id::{
    MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;
use tracing::Span;

/// Hard bound on a stored request id, in bytes. Inbound `x-request-id` values
/// longer than this are truncated; minted ids are 32 hex chars (16 bytes).
pub const MAX_REQUEST_ID_LEN: usize = 64;

/// Bounded request-id maker: mints a 32-char lowercase hex id from 16
/// getrandom bytes. Runs only when no inbound `x-request-id` exists —
/// `SetRequestIdLayer` skips the maker when the header is present and copies
/// the header verbatim (which [`bound_request_id`] has already truncated).
#[derive(Clone, Copy, Default)]
pub struct BoundedRequestId;

impl MakeRequestId for BoundedRequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).ok()?;
        HeaderValue::from_str(&hex_encode(&bytes))
            .ok()
            .map(RequestId::new)
    }
}

/// Hex-encode bytes as lowercase, 2 chars per byte.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Truncate an inbound request-id header value to [`MAX_REQUEST_ID_LEN`]
/// bytes. Returns `None` when the value already fits. The first 64 bytes of a
/// valid header value are always a valid header value; the fallback keeps the
/// original only as a defensive measure.
fn truncate_header_value(value: &HeaderValue) -> Option<HeaderValue> {
    let bytes = value.as_bytes();
    (bytes.len() > MAX_REQUEST_ID_LEN).then(|| {
        HeaderValue::from_bytes(&bytes[..MAX_REQUEST_ID_LEN]).unwrap_or_else(|_| value.clone())
    })
}

/// Outermost request-id bound: truncates an inbound `x-request-id` header to
/// [`MAX_REQUEST_ID_LEN`] bytes and pre-sets the bounded [`RequestId`]
/// extension so spans, request_log, and the propagated response header all
/// observe the bounded id. Runs before [`SetRequestIdLayer`]; when no inbound
/// header exists it does nothing and the set layer mints via
/// [`BoundedRequestId`].
pub async fn bound_request_id(mut request: Request<Body>, next: Next) -> Response<Body> {
    if let Some(value) = request.headers().get("x-request-id").cloned() {
        if let Some(bounded) = truncate_header_value(&value) {
            request
                .headers_mut()
                .insert("x-request-id", bounded.clone());
            request.extensions_mut().insert(RequestId::new(bounded));
        }
    }
    next.run(request).await
}

fn make_span<B>(request: &Request<B>) -> Span {
    // Read the effective id from the RequestId extension, which the bound
    // middleware + SetRequestIdLayer populated (inbound header wins, else
    // minted hex id). Fall back to the raw inbound header only when the set
    // layer is absent (e.g. unit tests). Never mint a second ID here —
    // SetRequestIdLayer owns generation.
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
/// same assembly; apply with
/// `.layer(propagate).layer(trace).layer(set)` and then
/// `.layer(axum::middleware::from_fn(bound_request_id))` outermost.
pub fn build_http_layers<B>() -> (
    SetRequestIdLayer<BoundedRequestId>,
    HttpTraceLayer<B>,
    PropagateRequestIdLayer,
) {
    (
        SetRequestIdLayer::x_request_id(BoundedRequestId),
        make_trace_layer(),
        PropagateRequestIdLayer::x_request_id(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Echoes the x-request-id header and the RequestId extension as JSON.
    async fn trace_echo_handler(req: Request<Body>) -> axum::response::Json<serde_json::Value> {
        let header = req
            .headers()
            .get("x-request-id")
            .map(|v| v.to_str().unwrap().to_owned());
        let ext = req
            .extensions()
            .get::<RequestId>()
            .map(|id| id.header_value().to_str().unwrap().to_owned());
        serde_json::json!({ "header": header, "ext": ext }).into()
    }

    fn echo_route() -> axum::Router {
        axum::Router::new().route("/", axum::routing::get(trace_echo_handler))
    }

    async fn body_text(response: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        String::from_utf8(bytes.to_vec()).expect("utf8 body")
    }

    #[test]
    fn inbound_id_longer_than_max_is_truncated_to_max() {
        let long = HeaderValue::from_str(&"x".repeat(200)).expect("header value");
        let bounded = truncate_header_value(&long).expect("long values are truncated");
        assert_eq!(bounded.as_bytes().len(), MAX_REQUEST_ID_LEN);
        assert!(bounded.as_bytes().iter().all(|&b| b == b'x'));
    }

    #[test]
    fn short_inbound_id_is_left_untouched() {
        let short = HeaderValue::from_static("abc-123");
        assert!(truncate_header_value(&short).is_none());
    }

    #[tokio::test]
    async fn long_inbound_header_and_extension_are_bounded() {
        // Same assembly as production: bound middleware (outermost, no set layer
        // — inbound wins) over the echo route.
        let app = echo_route().layer(axum::middleware::from_fn(bound_request_id));

        let long_id = "z".repeat(100);
        let response = app
            .oneshot(
                Request::builder()
                    .header("x-request-id", &long_id)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("call");
        let parsed: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("json body");
        let expected = "z".repeat(MAX_REQUEST_ID_LEN);
        assert_eq!(parsed["header"], serde_json::json!(expected));
        assert_eq!(parsed["ext"], serde_json::json!(expected));
    }

    #[tokio::test]
    async fn missing_header_mints_bounded_hex_in_header_and_extension() {
        // Same assembly as production: bound (outermost) + set layer.
        let app = echo_route()
            .layer(axum::middleware::from_fn(bound_request_id))
            .layer(SetRequestIdLayer::x_request_id(BoundedRequestId));

        let response = app
            .oneshot(Request::builder().body(Body::empty()).expect("request"))
            .await
            .expect("call");
        let parsed: serde_json::Value =
            serde_json::from_str(&body_text(response).await).expect("json body");
        let header = parsed["header"].as_str().expect("header id");
        let ext = parsed["ext"].as_str().expect("extension id");
        // The minted id lands in both the header and the extension.
        assert_eq!(header, ext);
        // 16 random bytes → 32 lowercase hex chars, well under the 64-byte cap.
        assert_eq!(header.len(), 32);
        assert!(header.bytes().all(|b| b.is_ascii_hexdigit()));
    }
}
