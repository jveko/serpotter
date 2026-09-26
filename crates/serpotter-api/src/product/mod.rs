//! Product HTTP shells: search, extract, research.

pub mod errors;
pub mod extract;
pub mod search;

use std::future::Future;
use std::ops::Deref;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::{BytesRejection, FailedToBufferBody, JsonRejection};
use axum::extract::{FromRequest, Json, Request};
use axum::http::StatusCode;
use serpotter_auth::problem_response;
use serpotter_db::TokenRow;
use serpotter_product::{ExecMeta, MetaSink, ProductCtx};

use crate::events::fields_from_meta;
use crate::AppState;

/// Default overall request deadline when `REQUEST_TIMEOUT_SECS` is unset
/// (F10). Any product call that exceeds this budget answers 504 /
/// MCP `Timeout`.
pub(crate) const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// `Json<T>` wrapper that maps every body-extraction rejection to the same
/// RFC 9457 problem+json shape the handlers use, so no rejection path leaks
/// a plain-text body (F00).
///
/// Mapping (stable kinds):
/// - malformed JSON                → 400 `InvalidJson`
/// - valid JSON, wrong shape (`{}`)→ 422 `InvalidJson`
/// - missing/invalid content-type  → 415 `InvalidContentType`
/// - body over `BODY_LIMIT_BYTES`  → 413 `BodyTooLarge`
pub struct AppJson<T>(pub T);

impl<T> Deref for AppJson<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Body extractor for the three PRODUCT routes that EMITS an event on
/// rejection.
///
/// [`AppJson`] answers the problem+json response but stays silent, so a
/// malformed body, a wrong-shape body, a missing content-type and an
/// over-limit body never reached the ring, the error window or the metrics —
/// exactly the requests an operator most needs to see. On rejection this
/// wrapper emits one event (same field shape as every other product event;
/// `attemptCount` 0 and no vendor, because the request never reached a
/// provider) and
/// returns the identical response [`AppJson`] would have.
///
/// Deliberately NOT the behaviour of the admin routes' shared `AppJson`: those
/// are not product requests and have no `RequestEvents` funnel.
pub struct AppJsonLogged<T>(pub T);

impl<T> Deref for AppJsonLogged<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> FromRequest<AppState> for AppJsonLogged<T>
where
    T: serde::de::DeserializeOwned,
{
    type Rejection = axum::response::Response;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        // Read the Parts extensions BEFORE the body is consumed: this is where
        // `ApiTokenLogged` stashed the authenticated token row, which the
        // handler (the only other holder) never receives once the body fails.
        let (parts, body) = req.into_parts();
        let token_name = parts
            .extensions
            .get::<TokenRow>()
            .map(|row| row.name.clone());
        let uri_path = parts.uri.path().to_string();
        let request_id = crate::events::request_id_from_headers(&parts.headers);
        let req = Request::from_parts(parts, body);
        match Json::<T>::from_request(req, state).await {
            Ok(json) => Ok(AppJsonLogged(json.0)),
            Err(rejection) => {
                let (response, kind) = json_rejection_problem(rejection);
                let status = response.status().as_u16() as i64;
                // Same field constructor every other product event uses; the
                // empty meta is honest (no vendor was ever reached) and
                // `fields_from_meta` leaves cost/token/provider fields empty
                // for it.
                let fields = fields_from_meta(
                    crate::events::static_product_path(&uri_path),
                    status,
                    Some(kind),
                    None,
                    request_id,
                    token_name,
                    None,
                    &ExecMeta::default(),
                );
                crate::events::emit(&state.events, fields, Instant::now());
                Err(response)
            }
        }
    }
}

impl<T, S> FromRequest<S> for AppJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(json) => Ok(AppJson(json.0)),
            Err(rejection) => Err(json_rejection_problem(rejection).0),
        }
    }
}

/// Map an axum `Json` rejection to the problem+json response AND its stable
/// kind, so a caller that also emits an event (see [`AppJsonLogged`]) cannot
/// name a different kind than the wire problem does.
fn json_rejection_problem(rejection: JsonRejection) -> (axum::response::Response, &'static str) {
    let (status, kind, detail) = match rejection {
        JsonRejection::JsonSyntaxError(e) => {
            (StatusCode::BAD_REQUEST, "InvalidJson", e.to_string())
        }
        JsonRejection::JsonDataError(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "InvalidJson",
            e.to_string(),
        ),
        JsonRejection::MissingJsonContentType(e) => (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "InvalidContentType",
            e.to_string(),
        ),
        JsonRejection::BytesRejection(BytesRejection::FailedToBufferBody(
            FailedToBufferBody::LengthLimitError(e),
        )) => (StatusCode::PAYLOAD_TOO_LARGE, "BodyTooLarge", e.to_string()),
        JsonRejection::BytesRejection(b) => (StatusCode::BAD_REQUEST, "InvalidJson", b.to_string()),
        // JsonRejection is non_exhaustive; unknown variants stay a 400 problem.
        other => (StatusCode::BAD_REQUEST, "InvalidJson", other.to_string()),
    };
    (problem_response(status, kind, detail), kind)
}

// --- F10 overall request deadline -------------------------------------------

/// Result of running a product future under the request deadline.
pub(crate) enum DeadlineOutcome<T> {
    /// The future finished within budget.
    Completed(T),
    /// The deadline elapsed first; the future was dropped. The payload is the
    /// live metadata the dropped future had published, so a 504 names the
    /// vendor/key/node/attempts it was on instead of "unknown, 0 attempts".
    Elapsed(ExecMeta),
}

/// Install a fresh live-metadata sink on `ctx`.
///
/// MUST be called by a handler BEFORE it builds the product future: the
/// future borrows `ctx`, so a sink installed afterwards — or onto a clone —
/// is never observed, and the `Elapsed` arm would fall back to an empty meta.
pub(crate) fn install_meta_sink(ctx: &mut ProductCtx) {
    ctx.meta_sink = Some(Arc::new(MetaSink::new()));
}

/// Run `fut` under `timeout`.
///
/// On elapse the future is dropped (key/node holds are released by their Drop
/// safety nets) and [`DeadlineOutcome::Elapsed`] carries the last snapshot
/// published to the [`ProductCtx::meta_sink`] installed by
/// [`install_meta_sink`]. The deadline is the ONE case where the product's own
/// `ExecMeta` is never returned to the caller, so that snapshot is the only
/// honest source of service/attemptCount/keyId/nodeId for the event.
pub(crate) async fn run_with_deadline<T, F>(
    timeout: Duration,
    ctx: &ProductCtx,
    fut: F,
) -> DeadlineOutcome<T>
where
    F: Future<Output = T>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(v) => DeadlineOutcome::Completed(v),
        Err(_elapsed) => DeadlineOutcome::Elapsed(
            ctx.meta_sink
                .as_ref()
                .and_then(|s| s.last())
                .unwrap_or_default(),
        ),
    }
}

/// Human detail for the 504 / MCP `Timeout` problem + envelope.
pub(crate) fn deadline_detail(timeout: Duration) -> String {
    if timeout.as_secs() >= 1 {
        format!("request exceeded {}s deadline", timeout.as_secs())
    } else {
        format!("request exceeded {}ms deadline", timeout.as_millis())
    }
}

/// Upper bound for `REQUEST_TIMEOUT_SECS` (24 h). Absurdly large deadlines
/// would panic at `Instant::now() + timeout` (deep research computes one
/// unguarded); values above the bound are rejected to the compiled default,
/// centrally, exactly like any other invalid setting.
const MAX_REQUEST_TIMEOUT_SECS: u64 = 86_400;

/// Parse `REQUEST_TIMEOUT_SECS`: positive integers up to
/// [`MAX_REQUEST_TIMEOUT_SECS`] (larger values are rejected to the default
/// with a warning);
/// anything else (unset, empty, non-numeric, zero) falls back to
/// [`DEFAULT_REQUEST_TIMEOUT`] with a warning.
pub(crate) fn parse_request_timeout(value: Option<&str>) -> Duration {
    match value {
        None => DEFAULT_REQUEST_TIMEOUT,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(secs) if secs > 0 && secs <= MAX_REQUEST_TIMEOUT_SECS => Duration::from_secs(secs),
            _ => {
                tracing::warn!(
                    value = %raw.trim(),
                    ?DEFAULT_REQUEST_TIMEOUT,
                    "invalid REQUEST_TIMEOUT_SECS; using default"
                );
                DEFAULT_REQUEST_TIMEOUT
            }
        },
    }
}

/// Read + parse `REQUEST_TIMEOUT_SECS` once when the process-wide product
/// configuration is first accessed. This function is the API crate's source of
/// truth for the timeout normalization mirrored by the key pool.
pub(crate) fn request_timeout_from_env() -> Duration {
    parse_request_timeout(std::env::var("REQUEST_TIMEOUT_SECS").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::capture_warns;
    use std::time::Instant;

    #[test]
    fn parse_request_timeout_warns_with_value_and_default_for_invalid_input() {
        for value in [Some(""), Some("abc"), Some("-5"), Some("0"), Some("86401")] {
            let (timeout, text) = capture_warns(|| parse_request_timeout(value));
            assert_eq!(timeout, DEFAULT_REQUEST_TIMEOUT);
            assert!(
                text.contains("invalid REQUEST_TIMEOUT_SECS; using default")
                    && text.contains("DEFAULT_REQUEST_TIMEOUT=120s"),
                "timeout warning must name the fallback: {value:?}: {text}"
            );
            // An empty override renders as an empty value field, so only
            // non-empty inputs can be checked for the raw value itself.
            if let Some(raw) = value.filter(|raw| !raw.trim().is_empty()) {
                assert!(
                    text.contains(&format!("value={}", raw.trim())),
                    "timeout warning must carry the raw offending value: {text}"
                );
            }
        }
    }

    #[test]
    fn parse_request_timeout_defaults_silently_when_unset() {
        let (timeout, text) = capture_warns(|| parse_request_timeout(None));
        assert_eq!(timeout, DEFAULT_REQUEST_TIMEOUT);
        assert!(text.is_empty(), "an unset variable must not warn: {text}");
    }

    #[test]
    fn parse_request_timeout_accepts_positive_seconds() {
        assert_eq!(parse_request_timeout(Some("1")), Duration::from_secs(1));
        assert_eq!(parse_request_timeout(Some(" 42 ")), Duration::from_secs(42));
    }

    #[test]
    fn parse_request_timeout_rejects_values_above_bound() {
        // The bound itself is accepted; anything above it behaves like an
        // invalid setting: warn + compiled default (never an
        // Instant-overflow deadline).
        assert_eq!(
            parse_request_timeout(Some("86400")),
            Duration::from_secs(86_400)
        );
        assert_eq!(
            parse_request_timeout(Some("86401")),
            DEFAULT_REQUEST_TIMEOUT
        );
        assert_eq!(
            parse_request_timeout(Some("18446744073709551615")),
            DEFAULT_REQUEST_TIMEOUT
        );
    }

    #[test]
    fn deadline_detail_renders_seconds_and_millis() {
        assert_eq!(
            deadline_detail(Duration::from_secs(120)),
            "request exceeded 120s deadline"
        );
        assert_eq!(
            deadline_detail(Duration::from_secs(1)),
            "request exceeded 1s deadline"
        );
        assert_eq!(
            deadline_detail(Duration::from_millis(500)),
            "request exceeded 500ms deadline"
        );
    }

    /// Minimal `ProductCtx` for the deadline tests: an in-memory DB and pinned
    /// dead providers — the wrapper itself only reads/writes `meta_sink`.
    async fn bare_ctx() -> ProductCtx {
        crate::test_support::app_state().await.product_ctx()
    }

    #[tokio::test]
    async fn run_with_deadline_completes_fast_future() {
        let ctx = bare_ctx().await;
        let out = run_with_deadline(Duration::from_secs(10), &ctx, async { 42 }).await;
        assert!(matches!(out, DeadlineOutcome::Completed(42)));
    }

    /// The attribution contract: a deadline that elapses mid-attempt reports
    /// the vendor/key/node the dropped future had published, not an empty
    /// meta (`service: null, attemptCount: 0`).
    #[tokio::test]
    async fn elapsed_deadline_reports_the_live_meta_snapshot() {
        let mut ctx = bare_ctx().await;
        install_meta_sink(&mut ctx);
        let sink = Arc::clone(ctx.meta_sink.as_ref().expect("sink installed"));
        let publisher = ctx.clone();
        let out = run_with_deadline(Duration::from_millis(50), &ctx, async move {
            let mut meta = ExecMeta::default();
            meta.note_attempt("tavily", 7, Some(3), false);
            publisher.observe_meta(&meta);
            tokio::time::sleep(Duration::from_secs(60)).await;
            42
        })
        .await;
        match out {
            DeadlineOutcome::Completed(_) => panic!("deadline must fire"),
            DeadlineOutcome::Elapsed(meta) => {
                assert_eq!(meta.providers_consulted, vec!["tavily".to_string()]);
                assert_eq!(meta.attempt_count, 1);
                assert_eq!(meta.key_id, Some(7));
                assert_eq!(meta.node_id, Some(3));
            }
        }
        assert!(
            sink.last().is_some(),
            "the sink outlives the dropped future"
        );
    }

    /// No attempt was ever published (the request died before any provider):
    /// the Elapsed arm falls back to an empty meta rather than inventing one.
    #[tokio::test]
    async fn elapsed_deadline_without_any_attempt_is_an_empty_meta() {
        let mut ctx = bare_ctx().await;
        install_meta_sink(&mut ctx);
        let out = run_with_deadline(
            Duration::from_millis(50),
            &ctx,
            tokio::time::sleep(Duration::from_secs(60)),
        )
        .await;
        assert!(matches!(
            out,
            DeadlineOutcome::Elapsed(ExecMeta {
                attempt_count: 0,
                ..
            })
        ));
    }

    /// A ctx with no sink installed must still answer (MCP/tool paths and any
    /// future caller that forgets to install one) instead of panicking.
    #[tokio::test]
    async fn elapsed_deadline_without_a_sink_is_an_empty_meta() {
        let ctx = bare_ctx().await;
        assert!(ctx.meta_sink.is_none());
        let out = run_with_deadline(
            Duration::from_millis(50),
            &ctx,
            tokio::time::sleep(Duration::from_secs(60)),
        )
        .await;
        assert!(matches!(
            out,
            DeadlineOutcome::Elapsed(ExecMeta {
                attempt_count: 0,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn run_with_deadline_elapses_on_slow_future() {
        let started = Instant::now();
        let ctx = bare_ctx().await;
        let out = run_with_deadline(
            Duration::from_millis(50),
            &ctx,
            tokio::time::sleep(Duration::from_secs(60)),
        )
        .await;
        assert!(matches!(out, DeadlineOutcome::Elapsed(_)));
        // The deadline must fire early, not wait for the inner future.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "deadline did not fire early"
        );
    }
}
