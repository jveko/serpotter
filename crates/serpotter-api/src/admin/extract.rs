//! Admin request extractors: every admin body/query/path rejection answers the
//! same RFC 9457 `application/problem+json` shape as the admin handlers, so a
//! malformed request never leaks axum's plain-text body.
//!
//! [`AppJson`] is the product wrapper (`crate::product::AppJson`); `AppQuery`
//! and `AppPath` are its query/path siblings here.
//!
//! Mapping (stable kinds):
//! - malformed JSON               → 400 `InvalidJson` (via `AppJson`)
//! - valid JSON, wrong shape      → 422 `InvalidJson` (via `AppJson`)
//! - missing/invalid content-type → 415 `InvalidContentType` (via `AppJson`)
//! - unparseable query string     → 400 `InvalidQuery`
//! - unparseable path segment     → 400 `InvalidPath`

use std::ops::Deref;

use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{FromRequestParts, Path, Query};
use axum::http::request::Parts;
use axum::http::StatusCode;
use serpotter_auth::problem_response;

pub use crate::product::AppJson;

/// `Query<T>` wrapper that maps a query-string rejection to a problem+json
/// response instead of axum's plain-text body.
pub struct AppQuery<T>(pub T);

impl<T> Deref for AppQuery<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, S> FromRequestParts<S> for AppQuery<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(query) => Ok(AppQuery(query.0)),
            Err(rejection) => Err(query_rejection_problem(rejection)),
        }
    }
}

fn query_rejection_problem(rejection: QueryRejection) -> axum::response::Response {
    // QueryRejection is non_exhaustive; unknown variants stay a 400 problem.
    match rejection {
        QueryRejection::FailedToDeserializeQueryString(e) => {
            problem_response(StatusCode::BAD_REQUEST, "InvalidQuery", e.to_string())
        }
        other => problem_response(StatusCode::BAD_REQUEST, "InvalidQuery", other.to_string()),
    }
}

/// `Path<T>` wrapper that maps a path-parameter rejection to problem+json.
pub struct AppPath<T>(pub T);

impl<T> Deref for AppPath<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, S> FromRequestParts<S> for AppPath<T>
where
    T: serde::de::DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = axum::response::Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(path) => Ok(AppPath(path.0)),
            Err(rejection) => Err(path_rejection_problem(rejection)),
        }
    }
}

fn path_rejection_problem(rejection: PathRejection) -> axum::response::Response {
    match rejection {
        PathRejection::FailedToDeserializePathParams(e) => {
            problem_response(StatusCode::BAD_REQUEST, "InvalidPath", e.to_string())
        }
        // Route config bug (a `Request` extracted before `Path`), not a client
        // error: keep it a 500, still problem+json.
        PathRejection::MissingPathParams(e) => problem_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "MissingPathParams",
            e.to_string(),
        ),
        other => problem_response(StatusCode::BAD_REQUEST, "InvalidPath", other.to_string()),
    }
}

// --- bounded admin string inputs -------------------------------------------

/// Longest admin-supplied free-form string accepted for a name, host or key.
/// 256 chars is far above any real label or vendor secret prefix and keeps a
/// 2 MiB body from being stored, echoed in `list_*` and pasted into
/// `reqwest::Proxy::all`.
pub(crate) const MAX_ADMIN_STRING_CHARS: usize = 256;

/// Reject an over-long admin string with the handler's own 400 problem shape.
/// Returns the trimmed value on success.
pub(crate) fn bounded_field<'a>(field: &'static str, value: &'a str) -> Result<&'a str, String> {
    let trimmed = value.trim();
    if trimmed.chars().count() > MAX_ADMIN_STRING_CHARS {
        return Err(format!(
            "{field} must be at most {MAX_ADMIN_STRING_CHARS} characters"
        ));
    }
    Ok(trimmed)
}

/// Syntax check for a node `host`: the value is interpolated raw into
/// `{protocol}://[user:pass@]host:port` for `reqwest::Proxy::all`, so a host
/// carrying a scheme, port, path, credentials or whitespace would silently
/// repoint the proxy (or fail per product request). Accepts a DNS name
/// (including a single label — `localhost` and intranet names resolve via
/// `/etc/hosts`/mDNS/private DNS), an IPv4 literal, or a bracketed IPv6
/// literal.
pub(crate) fn valid_node_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }

    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    // DNS name: one or more dot-separated LDH labels.
    if host.contains([':', '/', '?', '#', '@']) {
        return false;
    }
    for label in host.split('.') {
        if label.is_empty()
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return false;
        }
    }
    true
}

/// Bound a node credential stored as `Option<Option<String>>`: the outer
/// option is keep/change, the inner is clear/set — a set value is trimmed and
/// length-bounded to [`MAX_ADMIN_STRING_CHARS`], exactly like every other admin
/// string, so the credential-rotation path cannot store an unbounded value
/// either. The nesting is preserved so bounding never collapses "keep the
/// stored credential" into "clear it".
///
/// `field` names the field in the 400 detail so the message points at the
/// member the caller actually sent (`username` or `password`).
pub(crate) fn bounded_credentials<'a>(
    field: &'static str,
    value: Option<&'a Option<String>>,
) -> Result<Option<Option<&'a str>>, String> {
    match value {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(raw)) => bounded_field(field, raw).map(|v| Some(Some(v))),
    }
}

/// Problem detail for an admin `DatabaseError`. The real
/// [`serpotter_db::DbError`] text (SQL, table/column names, sometimes row
/// values) is logged server-side and never handed to an admin client; the
/// kind and status still say everything a caller needs. Mirrors
/// `product::errors::database_problem` so both surfaces share one promise.
const DATABASE_ERROR_DETAIL: &str = "internal storage error";

/// Log a `DatabaseError`'s real text server-side and answer the generic
/// admin problem. One writer, so "logged, never echoed" has a single owner.
pub(crate) fn database_problem(e: serpotter_db::DbError) -> axum::response::Response {
    tracing::error!(error = %e, "admin request failed with a database error");
    problem_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "DatabaseError",
        DATABASE_ERROR_DETAIL,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_field_trims_and_bounds() {
        assert_eq!(bounded_field("name", "  web-ui  ").unwrap(), "web-ui");
        let ok = "x".repeat(MAX_ADMIN_STRING_CHARS);
        assert_eq!(
            bounded_field("name", &ok).unwrap().len(),
            MAX_ADMIN_STRING_CHARS
        );
        let too_long = "x".repeat(MAX_ADMIN_STRING_CHARS + 1);
        let err = bounded_field("name", &too_long).unwrap_err();
        assert!(err.contains("name") && err.contains("256"), "detail: {err}");
    }

    #[test]
    fn valid_node_host_accepts_dns_and_ip_literals() {
        for good in [
            "proxy.example",
            "p.example.com",
            "my-proxy_1.example",
            // Single-label names resolve via /etc/hosts / mDNS / private DNS
            // and are perfectly dialable proxy authorities.
            "localhost",
            "privoxy",
            "127.0.0.1",
            "10.0.0.255",
            "[::1]",
            "[2001:db8::1]",
        ] {
            assert!(valid_node_host(good), "must accept {good}");
        }
    }

    #[test]
    fn valid_node_host_rejects_authority_smuggling() {
        for bad in [
            "",
            "http://proxy.example", // scheme
            "proxy.example:8080",   // port belongs in the port field
            "proxy.example/path",   // path
            "user@proxy.example",   // credentials belong in username/password
            "proxy example",        // whitespace
            "proxy.example?q=1",
            "[not-ipv6]",
            "[::1",
            ".proxy.example",
            "proxy.example.",
            "-proxy.example",
            "proxy-.example",
        ] {
            assert!(!valid_node_host(bad), "must reject {bad:?}");
        }
    }
}
