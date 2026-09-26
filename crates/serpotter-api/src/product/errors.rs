//! Shared product → HTTP / request_log status+kind mapping (REST + MCP).

use axum::http::StatusCode;
use serpotter_product::{ExtractError, ResearchError, SearchExecError};

/// `(http_status, log_status_i64, error_kind, detail)`.
pub type ProductProblem = (StatusCode, i64, &'static str, String);

/// Single source of truth for machine-readable retryability of a stable error
/// kind. A kind is retryable UNLESS retrying cannot help:
/// - `ValidationError` — a client-side request-shape failure; the same
///   request will fail identically.
/// - `DatabaseError` — our own storage fault, not a vendor or capacity blip.
///   The driver text (schema, table, sometimes row values) stays server-side;
///   telling a caller "retryable" would have them hammer a broken database.
/// - `NotReady` — a deployment fault: the database schema is older than the
///   build expects, and only a migration fixes it. Retrying the identical call
///   fails identically, so `retryable: true` would point an agent at a retry
///   loop that can never succeed. (MCP-only tag: the REST surface reports the
///   same condition as a bare `503 /ready` with no kind.)
///
/// Every other 5xx/timeout kind (NoHealthyKey/KeyBusy/NoHealthyNode/
/// ProviderError/SearchError/ExtractTimeout/RequestTimeout) is transient, as
/// are the MCP-level `Timeout`/`Cancelled`/`InternalError` tags.
pub fn kind_retryable(kind: &str) -> bool {
    !matches!(kind, "ValidationError" | "DatabaseError" | "NotReady")
}

/// Problem detail for a `DatabaseError`. The real [`serpotter_db::DbError`]
/// text (SQL, table/column names, sometimes row values) is logged
/// server-side and never handed to a token holder; the kind, status and
/// `retryable:false` still say everything a client needs to act on.
const DATABASE_ERROR_DETAIL: &str = "internal storage error";

/// Log a `DatabaseError`'s real text server-side and return the generic
/// problem detail. One writer, so the "logged, never echoed" promise has a
/// single owner.
fn database_problem(e: serpotter_db::DbError) -> ProductProblem {
    tracing::error!(error = %e, "product request failed with a database error");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        500,
        "DatabaseError",
        DATABASE_ERROR_DETAIL.to_string(),
    )
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
        // Client-side request-shape error on the search path: a parameter our
        // own guards refused (pre-lease gate, or every leg refusing it). 400,
        // never a 502 — symmetric with extract's `InvalidRequest` mapping.
        SearchExecError::InvalidRequest(m) => (StatusCode::BAD_REQUEST, 400, "ValidationError", m),
        SearchExecError::Search(m) => (StatusCode::BAD_GATEWAY, 502, "SearchError", m),
        SearchExecError::Db(e) => database_problem(e),
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
        ExtractError::Db(e) => database_problem(e),
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
        SearchExecError::InvalidRequest(_) => (400, "ValidationError"),
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

    /// The search twin of `structured_invalid_provider_is_400_validation`, on
    /// BOTH surfaces and with the two strings the product layer actually emits:
    /// the pre-lease shape gate in `search_inner`, and a provider guard's
    /// refusal (surfaced only once every leg refused). Never a retryable 502.
    #[test]
    fn search_invalid_request_is_400_validation() {
        for detail in [
            "domain filters must be bare hostnames (e.g. \"example.com\"), got \
             \"[\\\"a\\\", \\\"b\\\"]\"",
            "tavily search unsupported: country must be a full country name \
             (e.g. \"Indonesia\"), got a value Tavily rejects",
        ] {
            assert_eq!(
                search_err_log(&SearchExecError::InvalidRequest(detail.into())),
                (400, "ValidationError"),
                "MCP/log tag for: {detail}"
            );
            let (code, st, kind, d) =
                search_problem(SearchExecError::InvalidRequest(detail.into()));
            assert_eq!(code, StatusCode::BAD_REQUEST, "wire status: {d}");
            assert_eq!((st, kind, d.as_str()), (400, "ValidationError", detail));
            // The MCP envelope derives `retryable` from this kind tag, so the
            // class is non-retryable on both surfaces by construction.
            assert!(!kind_retryable(kind));
        }
    }

    /// The boundary that keeps the fix honest: a vendor-rejected status is a
    /// provider-side fact, so an upstream 400 (our own payload bugs — the
    /// Firecrawl `maxAge` wave) and an upstream 402 (out of credit, whose
    /// `PaymentRequired` disposition is about the KEY, not the caller) stay
    /// 502 `ProviderError` → `retryable:true` on both surfaces.
    #[test]
    fn search_vendor_rejected_statuses_map_to_provider_error() {
        for detail in [
            "tavily upstream error (status 400)",
            "exa is out of credits (upstream 402)",
            "tavily rate-limited (upstream 429); try again shortly",
        ] {
            let e = SearchExecError::Provider(detail.into());
            assert_eq!(
                search_err_log(&e),
                (502, "ProviderError"),
                "log/MCP tag for: {detail}"
            );
            let (code, st, kind, d) = search_problem(e);
            assert_eq!(code, StatusCode::BAD_GATEWAY, "wire status: {d}");
            assert_eq!((st, kind, d.as_str()), (502, "ProviderError", detail));
            assert!(kind_retryable(kind), "{detail} must stay retryable");
        }
    }

    /// Boundary guard: every failure the provider or the pool caused keeps its
    /// 502/503 class (and therefore `retryable:true`) — the new variant must
    /// not bleed into the genuine-outage paths.
    #[test]
    fn search_provider_side_kinds_keep_5xx() {
        let cases = [
            (
                SearchExecError::Provider("exa upstream error (status 401)".into()),
                502,
                "ProviderError",
            ),
            (
                SearchExecError::Provider("exa deep upstream error (status 402)".into()),
                502,
                "ProviderError",
            ),
            (
                SearchExecError::NoHealthyKey("no tavily key".into()),
                503,
                "NoHealthyKey",
            ),
            (SearchExecError::KeyBusy("all busy".into()), 503, "KeyBusy"),
            (
                SearchExecError::NoHealthyNode("no proxy node".into()),
                503,
                "NoHealthyNode",
            ),
        ];
        for (e, st, kind) in cases {
            assert_eq!(search_err_log(&e), (st, kind));
            let (code, log_st, k, _) = search_problem(e);
            assert_eq!(code.as_u16() as i64, st, "wire status for {kind}");
            assert_eq!(log_st, st);
            assert_eq!(k, kind);
            assert!(kind_retryable(k), "{kind} must stay retryable");
        }
    }

    /// `ResearchError` wraps search with `#[from]`, so research inherits the
    /// 400 without a mapping change of its own (on both surfaces).
    #[test]
    fn research_nests_invalid_request_as_400() {
        let detail = "domain filters must be bare hostnames (e.g. \"example.com\"), \
                      got \"not a host\"";
        let e = ResearchError::Search(SearchExecError::InvalidRequest(detail.into()));
        assert_eq!(research_err_log(&e), (400, "ValidationError"));
        let (code, st, kind, _) = research_problem(e);
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!((st, kind), (400, "ValidationError"));
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

    /// Retryability is derived from the KIND, so the contract is pinned as a
    /// table: the two non-retryable kinds are the ones a retry cannot help
    /// (a client-shape refusal, and our own storage fault — telling a caller
    /// to retry a broken database amplifies the outage). Everything else,
    /// including every vendor/capacity/timeout kind, stays retryable.
    #[test]
    fn kind_retryable_excludes_client_and_storage_faults() {
        for kind in [
            "NoHealthyKey",
            "KeyBusy",
            "NoHealthyNode",
            "ProviderError",
            "SearchError",
            "ExtractTimeout",
            "RequestTimeout",
            "Timeout",
            "Cancelled",
            "InternalError",
        ] {
            assert!(kind_retryable(kind), "kind {kind} should be retryable");
        }
        assert!(!kind_retryable("ValidationError"));
        assert!(!kind_retryable("DatabaseError"));
        // A schema-behind deployment is a deployment fault: retrying the same
        // call cannot migrate the database.
        assert!(!kind_retryable("NotReady"));
    }

    /// The wire must not carry SQL. `DbError` is transparent over `sqlx`, so
    /// its `Display` names tables, columns and sometimes bound values; both
    /// surfaces answer a fixed detail string while kind/status stay exact.
    #[test]
    fn database_error_detail_is_generic_and_non_retryable() {
        let cases = [
            search_problem(SearchExecError::Db(db_err())),
            extract_problem(ExtractError::Db(db_err())),
            research_problem(ResearchError::Search(SearchExecError::Db(db_err()))),
        ];
        for (code, status, kind, detail) in cases {
            assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
            assert_eq!((status, kind), (500, "DatabaseError"));
            assert_eq!(detail, DATABASE_ERROR_DETAIL);
            assert!(
                !detail.contains("no such table") && !detail.contains("SELECT"),
                "the driver text must never reach the wire: {detail}"
            );
            assert!(
                !kind_retryable(kind),
                "a storage fault is not retryable: {kind}"
            );
        }
    }

    /// The real error is still classified, not swallowed: the MCP/log tag
    /// keeps the exact 500/DatabaseError pair on every surface.
    #[test]
    fn database_err_log_kind_is_still_500() {
        let e = SearchExecError::Db(db_err());
        assert_eq!(search_err_log(&e), (500, "DatabaseError"));
        let e = ExtractError::Db(db_err());
        assert_eq!(extract_err_log(&e), (500, "DatabaseError"));
        let e = ResearchError::Extract(ExtractError::Db(db_err()));
        assert_eq!(research_err_log(&e), (500, "DatabaseError"));
    }

    fn db_err() -> serpotter_db::DbError {
        serpotter_db::DbError::Sqlx(sqlx::Error::RowNotFound)
    }
}
