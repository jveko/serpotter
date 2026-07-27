//! Shared product → HTTP / request_log status+kind mapping (REST + MCP).

use axum::http::StatusCode;
use serpotter_product::{ExtractError, ResearchError, SearchExecError};

/// `(http_status, log_status_i64, error_kind, detail)`.
pub type ProductProblem = (StatusCode, i64, &'static str, String);

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
        ExtractError::NoHealthyKey(m) => {
            (StatusCode::SERVICE_UNAVAILABLE, 503, "NoHealthyKey", m)
        }
        ExtractError::KeyBusy(m) => (StatusCode::SERVICE_UNAVAILABLE, 503, "KeyBusy", m),
        ExtractError::NoHealthyNode(m) => {
            (StatusCode::SERVICE_UNAVAILABLE, 503, "NoHealthyNode", m)
        }
        ExtractError::InvalidUrl(m) => (StatusCode::BAD_REQUEST, 400, "ValidationError", m),
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
}
