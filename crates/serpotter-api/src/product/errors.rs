//! Shared product → HTTP / request_log status+kind mapping (REST + MCP).

use axum::http::StatusCode;
use serpotter_product::{ExtractError, ResearchError, SearchExecError};

/// `(http_status, log_status_i64, error_kind, detail)`.
pub type ProductProblem = (StatusCode, i64, &'static str, String);

/// Single source of truth for machine-readable retryability of a stable error
/// kind. A kind is retryable UNLESS it is a client-side validation failure —
/// every 5xx/timeout kind (NoHealthyKey/KeyBusy/NoHealthyNode/ProviderError/
/// SearchError/DatabaseError/ExtractTimeout/RequestTimeout) is treated as
/// transient, including MCP-level `Timeout`/`Cancelled`/`InternalError`.
pub fn kind_retryable(kind: &str) -> bool {
    kind != "ValidationError"
}

pub fn search_problem(e: SearchExecError) -> ProductProblem {
    match e {
        SearchExecError::NoHealthyKey(m) => {
            (StatusCode::SERVICE_UNAVAILABLE, 503, "NoHealthyKey", m)
        }
        SearchExecError::KeyBusy(m) => (StatusCode::SERVICE_UNAVAILABLE, 503, "KeyBusy", m),
        SearchExecError::NoHealthyNode(m) => {
            (StatusCode::SERVICE_UNAVAILABLE, 503, "NoHealthyNode", m)
        }
        SearchExecError::Provider(m) => (StatusCode::BAD_GATEWAY, 502, "ProviderError", m),
        SearchExecError::Search(m) => (StatusCode::BAD_GATEWAY, 502, "SearchError", m),
        SearchExecError::Db(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            500,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub fn extract_problem(e: ExtractError) -> ProductProblem {
    match e {
        ExtractError::NoHealthyKey(m) => (StatusCode::SERVICE_UNAVAILABLE, 503, "NoHealthyKey", m),
        ExtractError::KeyBusy(m) => (StatusCode::SERVICE_UNAVAILABLE, 503, "KeyBusy", m),
        ExtractError::NoHealthyNode(m) => {
            (StatusCode::SERVICE_UNAVAILABLE, 503, "NoHealthyNode", m)
        }
        ExtractError::InvalidUrl(m) => (StatusCode::BAD_REQUEST, 400, "ValidationError", m),
        // B18 client-side request-shape error (structured with a non-firecrawl
        // provider) is a 400, never a provider 5xx.
        ExtractError::InvalidRequest(m) => (StatusCode::BAD_REQUEST, 400, "ValidationError", m),
        // B18: the bounded in-request poll window elapsed without a terminal
        // vendor job state — honest 504, distinct from the F10 request
        // deadline (RequestTimeout) so operators can tell the two apart.
        ExtractError::ExtractTimeout(m) => (StatusCode::GATEWAY_TIMEOUT, 504, "ExtractTimeout", m),
        ExtractError::Provider(m) => (StatusCode::BAD_GATEWAY, 502, "ProviderError", m),
        ExtractError::Db(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            500,
            "DatabaseError",
            e.to_string(),
        ),
    }
}

pub fn research_problem(e: ResearchError) -> ProductProblem {
    match e {
        ResearchError::Search(s) => search_problem(s),
        ResearchError::Extract(x) => extract_problem(x),
    }
}

/// MCP / log-only: status code as i64 + stable kind tag (match by ref — no Db remap).
pub fn search_err_log(e: &SearchExecError) -> (i64, &'static str) {
    match e {
        SearchExecError::NoHealthyKey(_) => (503, "NoHealthyKey"),
        SearchExecError::KeyBusy(_) => (503, "KeyBusy"),
        SearchExecError::NoHealthyNode(_) => (503, "NoHealthyNode"),
        SearchExecError::Provider(_) => (502, "ProviderError"),
        SearchExecError::Search(_) => (502, "SearchError"),
        SearchExecError::Db(_) => (500, "DatabaseError"),
    }
}

pub fn extract_err_log(e: &ExtractError) -> (i64, &'static str) {
    match e {
        ExtractError::NoHealthyKey(_) => (503, "NoHealthyKey"),
        ExtractError::KeyBusy(_) => (503, "KeyBusy"),
        ExtractError::NoHealthyNode(_) => (503, "NoHealthyNode"),
        ExtractError::InvalidUrl(_) => (400, "ValidationError"),
        ExtractError::InvalidRequest(_) => (400, "ValidationError"),
        ExtractError::ExtractTimeout(_) => (504, "ExtractTimeout"),
        ExtractError::Provider(_) => (502, "ProviderError"),
        ExtractError::Db(_) => (500, "DatabaseError"),
    }
}

pub fn research_err_log(e: &ResearchError) -> (i64, &'static str) {
    match e {
        ResearchError::Search(s) => search_err_log(s),
        ResearchError::Extract(x) => extract_err_log(x),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_tags_match_wire() {
        let (code, st, kind, _) = search_problem(SearchExecError::KeyBusy("busy".into()));
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(st, 503);
        assert_eq!(kind, "KeyBusy");
    }

    #[test]
    fn extract_invalid_url_is_400() {
        let (code, st, kind, _) = extract_problem(ExtractError::InvalidUrl("bad".into()));
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(st, 400);
        assert_eq!(kind, "ValidationError");
    }

    #[test]
    fn structured_invalid_provider_is_400_validation() {
        let (code, st, kind, _) = extract_problem(ExtractError::InvalidRequest(
            "structured extraction requires provider=firecrawl".into(),
        ));
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(st, 400);
        assert_eq!(kind, "ValidationError");
        assert_eq!(
            extract_err_log(&ExtractError::InvalidRequest("x".into())),
            (400, "ValidationError")
        );
    }

    #[test]
    fn structured_poll_timeout_is_504_extract_timeout() {
        let (code, st, kind, _) = extract_problem(ExtractError::ExtractTimeout(
            "firecrawl structured extraction did not finish within 90s".into(),
        ));
        assert_eq!(code, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(st, 504);
        assert_eq!(kind, "ExtractTimeout");
        assert_eq!(
            extract_err_log(&ExtractError::ExtractTimeout("t".into())),
            (504, "ExtractTimeout")
        );
    }

    #[test]
    fn research_nests_search() {
        let (code, st, kind, _) = research_problem(ResearchError::Search(
            SearchExecError::NoHealthyNode("n".into()),
        ));
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(st, 503);
        assert_eq!(kind, "NoHealthyNode");
    }

    #[test]
    fn err_log_db_stays_500() {
        // Tag-only path must not remap Db → Search/Provider.
        // We can't easily construct DbError; assert string arms + status table.
        let e = SearchExecError::Search("x".into());
        assert_eq!(search_err_log(&e), (502, "SearchError"));
        let e = ExtractError::InvalidUrl("u".into());
        assert_eq!(extract_err_log(&e), (400, "ValidationError"));
    }

    #[test]
    fn kind_retryable_only_excludes_validation() {
        // 5xx/timeout kinds are transient → retryable.
        for kind in [
            "NoHealthyKey",
            "KeyBusy",
            "NoHealthyNode",
            "ProviderError",
            "SearchError",
            "DatabaseError",
            "ExtractTimeout",
            "RequestTimeout",
            "Timeout",
            "Cancelled",
            "InternalError",
        ] {
            assert!(kind_retryable(kind), "kind {kind} should be retryable");
        }
        // Client-side validation is the single non-retryable kind.
        assert!(!kind_retryable("ValidationError"));
    }
}
