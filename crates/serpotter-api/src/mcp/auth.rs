use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::events;
use crate::{require_api_token, AppState};

use super::admission::Admission;
use super::MCP_SESSION_HEADER;

/// Event path label for the MCP transport. This layer runs INSIDE
/// `nest_service("/mcp", …)`, which strips the prefix from the URI it hands
/// over, so the mount point is the only stable label.
const MCP_EVENT_PATH: &str = "/mcp";

/// Body of rmcp's own "unknown session" 404, reused verbatim so a foreign
/// token cannot distinguish its rejection from an id that never existed.
const SESSION_NOT_FOUND_BODY: &str = "Not Found: Session not found";

/// Middleware state: the app (auth + events) plus the process-local
/// admission map this service was built with. A custom state type rather than
/// a new `AppState` field, so every existing `AppState` literal — including
/// the integration fixtures — keeps compiling unchanged.
#[derive(Clone)]
pub(super) struct McpLayerState {
    pub(super) app: AppState,
    pub(super) admission: Arc<Admission>,
}

pub async fn mcp_auth_middleware(
    State(state): State<McpLayerState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    // A CORS preflight carries no credentials — that is the entire point of a
    // preflight — so requiring a token here answered 401 to every browser and
    // made `MCP_ALLOWED_ORIGINS` unusable. Preflight is unauthenticated and
    // carries no MCP semantics; the real request that follows it still has to
    // authenticate. The CORS layer (installed OUTSIDE this one) short-circuits
    // the preflight into its own response, so this arm only runs when no CORS
    // allowlist is configured.
    if request.method() == Method::OPTIONS {
        return next.run(request).await;
    }
    let app = &state.app;
    let row = match require_api_token(app, request.headers()).await {
        Ok(row) => row,
        Err(r) => {
            // F08 parity: REST's `ApiTokenLogged` extractor emits an event for
            // every rejected token; MCP used to answer 401 with no ring row, no
            // error-window bucket and no metric, so a client hammering /mcp
            // with a bad token was invisible to the operator. Same field
            // constructor as the REST path, so the two cannot drift.
            // `token_name` stays None: the token does not exist.
            //
            // The path label is the MOUNT POINT, not `request.uri().path()`:
            // this layer is installed by `nest_service("/mcp", …)`, which
            // strips the prefix, so the inner URI is `/` and would file every
            // MCP auth failure under the REST catch-all `/api`.
            events::emit(
                &app.events,
                events::auth_failure_fields_for(MCP_EVENT_PATH, request.headers()),
                Instant::now(),
            );
            return r;
        }
    };
    let is_delete = request.method() == Method::DELETE;
    // A request that names a session must be driven by the token that opened
    // it. `initialize` mints the id and binds it AFTER rmcp answers (below);
    // every later POST/GET/DELETE must match.
    //
    // A mismatch answers the SAME 404 an unknown id gets — never 403, which
    // would confirm the id exists and turn the header into a probe for other
    // tenants' handles.
    let session = session_id(&request);
    if let Some(sid) = session.as_deref() {
        if state.admission.session_token(sid) != Some(row.id) {
            return session_not_found();
        }
    }
    // Tools read TokenRow via rmcp Extension<Parts> (Parts.extensions).
    request.extensions_mut().insert(row.clone());
    let response = next.run(request).await;
    // Post-response bookkeeping, two independent facts:
    //
    // 1. The request named NO session and the response carries one — that is
    //    `initialize` minting an id, so this token owns it. Gating on the
    //    ABSENCE of an inbound header is the point: a request that already
    //    carried a session id was ownership-checked above, and rebinding it
    //    here would be a no-op anyway (first writer wins).
    // 2. An ownership-verified DELETE the transport confirmed: the session is
    //    gone server-side, so the binding goes too. Only on success — a
    //    failed delete would otherwise orphan a session its owner can no
    //    longer reach.
    if session.is_none() && response.status().is_success() {
        if let Some(minted) = response
            .headers()
            .get(MCP_SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
        {
            state.admission.bind_session(minted, row.id);
        }
    } else if is_delete && response.status().is_success() {
        if let Some(sid) = session.as_deref() {
            state.admission.forget_session(sid);
        }
    }
    response
}

/// The `Mcp-Session-Id` header value, when present and well-formed.
fn session_id(request: &Request<Body>) -> Option<String> {
    request
        .headers()
        .get(MCP_SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The response rmcp builds for an unknown session id, reproduced exactly:
/// status 404, that same plain-text body, and **no** `Content-Type`.
///
/// Parity is the whole point. rmcp answers an unknown id from
/// `Response::builder().status(404).body("Not Found: Session not found")`
/// with no content-type set; adding one here — or answering
/// `application/problem+json` — would make the ownership rejection
/// distinguishable from "no such session", turning the header into an
/// existence probe for other tenants' handles. `mcp_runtime.rs` pins the
/// parity end-to-end by comparing this response against rmcp's own.
fn session_not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(axum::body::Body::from(SESSION_NOT_FOUND_BODY))
        .expect("static 404 response is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 404 a foreign token gets must carry rmcp's exact body and no extra
    /// header — the shape is pinned end-to-end against rmcp's own response in
    /// `tests/mcp_runtime.rs`; this guards the constructor itself.
    #[tokio::test]
    async fn foreign_session_404_carries_no_content_type() {
        let res = session_not_found();
        assert_eq!(res.status(), 404);
        assert!(
            res.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .is_none(),
            "rmcp's own 404 sets no Content-Type; matching it is the point"
        );
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(body.as_ref(), SESSION_NOT_FOUND_BODY.as_bytes());
    }
}
