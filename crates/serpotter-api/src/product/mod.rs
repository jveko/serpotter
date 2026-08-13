//! Product HTTP shells: search, extract, research.

pub mod errors;
pub mod extract;
pub mod search;

use std::future::Future;
use std::ops::Deref;
use std::time::Duration;

use axum::extract::rejection::{BytesRejection, FailedToBufferBody, JsonRejection};
use axum::extract::{FromRequest, Json, Request};
use axum::http::StatusCode;
use serpotter_auth::problem_response;

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

impl<T, S> FromRequest<S> for AppJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(json) => Ok(AppJson(json.0)),
            Err(rejection) => Err(json_rejection_problem(rejection)),
        }
    }
}

fn json_rejection_problem(rejection: JsonRejection) -> axum::response::Response {
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
    problem_response(status, kind, detail)
}

// --- F10 overall request deadline -------------------------------------------

/// Result of running a product future under the request deadline.
pub(crate) enum DeadlineOutcome<T> {
    /// The future finished within budget.
    Completed(T),
    /// The deadline elapsed first; the future was dropped.
    Elapsed,
}

/// Run `fut` under `timeout`. On elapse the future is dropped (key/node
/// holds are released by their Drop safety nets) and `Elapsed` is returned.
pub(crate) async fn run_with_deadline<T, F>(timeout: Duration, fut: F) -> DeadlineOutcome<T>
where
    F: Future<Output = T>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(v) => DeadlineOutcome::Completed(v),
        Err(_elapsed) => DeadlineOutcome::Elapsed,
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

/// Parse `REQUEST_TIMEOUT_SECS`: positive integers only; anything else
/// (unset, empty, non-numeric, zero) falls back to
/// [`DEFAULT_REQUEST_TIMEOUT`] with a warning.
pub(crate) fn parse_request_timeout(value: Option<&str>) -> Duration {
    match value {
        None => DEFAULT_REQUEST_TIMEOUT,
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(secs) if secs > 0 => Duration::from_secs(secs),
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

/// Read + parse `REQUEST_TIMEOUT_SECS` at call time (once per product_ctx()).
pub(crate) fn request_timeout_from_env() -> Duration {
    parse_request_timeout(std::env::var("REQUEST_TIMEOUT_SECS").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn parse_request_timeout_defaults_when_unset_or_invalid() {
        assert_eq!(parse_request_timeout(None), DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(parse_request_timeout(Some("")), DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(parse_request_timeout(Some("abc")), DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(parse_request_timeout(Some("-5")), DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(parse_request_timeout(Some("0")), DEFAULT_REQUEST_TIMEOUT);
    }

    #[test]
    fn parse_request_timeout_accepts_positive_seconds() {
        assert_eq!(parse_request_timeout(Some("1")), Duration::from_secs(1));
        assert_eq!(parse_request_timeout(Some(" 42 ")), Duration::from_secs(42));
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

    #[tokio::test]
    async fn run_with_deadline_completes_fast_future() {
        let out = run_with_deadline(Duration::from_secs(10), async { 42 }).await;
        assert!(matches!(out, DeadlineOutcome::Completed(42)));
    }

    #[tokio::test]
    async fn run_with_deadline_elapses_on_slow_future() {
        let started = Instant::now();
        let out = run_with_deadline(
            Duration::from_millis(50),
            tokio::time::sleep(Duration::from_secs(60)),
        )
        .await;
        assert!(matches!(out, DeadlineOutcome::Elapsed));
        // The deadline must fire early, not wait for the inner future.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "deadline did not fire early"
        );
    }
}
