//! API token generation, extraction, and RFC 9457 problem details.

use axum::http::{header, header::HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;

pub const TOKEN_PREFIX: &str = "tok-";
pub const TOKEN_RANDOM_BYTES: usize = 32;
pub const ERROR_TYPE_BASE: &str = "https://serpotter.dev/errors";

/// Mint a new API token: `tok-` + base64url(32 random bytes), no padding.
pub fn generate_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    getrandom::fill(&mut bytes)?;
    let suffix = URL_SAFE_NO_PAD.encode(bytes);
    Ok(format!("{TOKEN_PREFIX}{suffix}"))
}

/// Mint an opaque admin session token: `adm-` + base64url(32 random bytes).
pub fn generate_session_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; TOKEN_RANDOM_BYTES];
    getrandom::fill(&mut bytes)?;
    let suffix = URL_SAFE_NO_PAD.encode(bytes);
    Ok(format!("adm-{suffix}"))
}

/// Extract token from headers. Priority: `Authorization: Bearer` (auth-scheme is
/// case-insensitive per RFC 7235) then `x-api-key`. A non-Bearer Authorization
/// scheme falls through to `x-api-key`.
pub fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        if let Ok(s) = auth.to_str() {
            if let Some((scheme, rest)) = s.split_once(' ') {
                if scheme.eq_ignore_ascii_case("bearer") {
                    let t = rest.trim();
                    if !t.is_empty() {
                        return Some(t.to_string());
                    }
                }
            }
        }
    }
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(s) = key.to_str() {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct ProblemDetails {
    #[serde(rename = "type")]
    pub type_uri: String,
    pub title: String,
    pub status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Build an RFC 9457 problem body for REST auth failures (search-path shape).
pub fn problem_details(status: StatusCode, tag: &str, detail: impl Into<String>) -> ProblemDetails {
    let title = tag
        .chars()
        .enumerate()
        .flat_map(|(i, c)| {
            if i > 0 && c.is_uppercase() {
                vec![' ', c]
            } else {
                vec![c]
            }
        })
        .collect::<String>();
    ProblemDetails {
        type_uri: format!("{ERROR_TYPE_BASE}/{tag}"),
        title,
        status: status.as_u16(),
        detail: Some(detail.into()),
    }
}

/// 401 AuthenticationError with `Content-Type: application/problem+json`.
pub fn authentication_error(detail: impl Into<String>) -> Response {
    problem_response(StatusCode::UNAUTHORIZED, "AuthenticationError", detail)
}

/// Build an RFC 9457 problem+json `Response` with an explicit `tag` kind and
/// `Content-Type: application/problem+json`.
pub fn problem_response(status: StatusCode, tag: &str, detail: impl Into<String>) -> Response {
    let body = problem_details(status, tag, detail);
    let mut res = (status, Json(body)).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    res
}

/// `problem_response` with extra RFC 9457 extension members merged into the
/// body (e.g. machine-readable `retryable` for product-mapped problems).
/// Existing `problem_response` callers (auth/admin) are untouched — only
/// product handlers opt in via this sibling fn.
pub fn problem_response_ext(
    status: StatusCode,
    tag: &str,
    detail: impl Into<String>,
    extra: &[(&str, serde_json::Value)],
) -> Response {
    let mut body = serde_json::to_value(problem_details(status, tag, detail))
        .unwrap_or_else(|_| serde_json::json!({}));
    if let serde_json::Value::Object(map) = &mut body {
        for (k, v) in extra {
            map.insert(k.to_string(), v.clone());
        }
    }
    let mut res = (status, Json(body)).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_token_has_prefix_and_length() {
        let t = generate_token().expect("rng");
        assert!(t.starts_with("tok-"));
        // 32 bytes → 43 base64url chars without padding
        assert_eq!(t.len(), 4 + 43);
        assert!(!t.contains('+'));
        assert!(!t.contains('/'));
        assert!(!t.contains('='));
    }

    #[test]
    fn extract_prefers_bearer_over_x_api_key() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer tok-from-bearer"),
        );
        h.insert("x-api-key", HeaderValue::from_static("tok-from-header"));
        assert_eq!(extract_token(&h).as_deref(), Some("tok-from-bearer"));
    }

    #[test]
    fn extract_x_api_key_when_no_bearer() {
        let mut h = HeaderMap::new();
        h.insert("x-api-key", HeaderValue::from_static(" tok-key "));
        assert_eq!(extract_token(&h).as_deref(), Some("tok-key"));
    }

    #[test]
    fn extract_lowercase_bearer() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("bearer tok-lower"),
        );
        h.insert("x-api-key", HeaderValue::from_static("tok-header"));
        assert_eq!(extract_token(&h).as_deref(), Some("tok-lower"));
    }

    #[test]
    fn extract_mixed_case_bearer() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("BeArEr tok-mixed"),
        );
        assert_eq!(extract_token(&h).as_deref(), Some("tok-mixed"));
    }

    #[test]
    fn non_bearer_scheme_falls_through_to_x_api_key() {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwYXNz"),
        );
        h.insert("x-api-key", HeaderValue::from_static("tok-header"));
        assert_eq!(extract_token(&h).as_deref(), Some("tok-header"));
    }

    #[test]
    fn extract_none_when_missing() {
        let h = HeaderMap::new();
        assert!(extract_token(&h).is_none());
    }

    #[test]
    fn problem_details_auth_shape() {
        let p = problem_details(
            StatusCode::UNAUTHORIZED,
            "AuthenticationError",
            "Missing API token",
        );
        assert_eq!(
            p.type_uri,
            "https://serpotter.dev/errors/AuthenticationError"
        );
        assert_eq!(p.title, "Authentication Error");
        assert_eq!(p.status, 401);
        assert_eq!(p.detail.as_deref(), Some("Missing API token"));
    }
}
