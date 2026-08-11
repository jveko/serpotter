//! Product-layer errors (search / extract / research). Handlers map these to problem details.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchExecError {
    #[error("{0}")]
    NoHealthyKey(String),
    /// Keys exist but shared-cap acquire timed out (all at max_inflight).
    #[error("{0}")]
    KeyBusy(String),
    /// Fail-closed egress when `REQUIRE_OUTBOUND_PROXY` and no healthy node lease.
    #[error("{0}")]
    NoHealthyNode(String),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    Search(String),
    #[error(transparent)]
    Db(#[from] serpotter_db::DbError),
}

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("{0}")]
    NoHealthyKey(String),
    /// Keys exist but shared-cap acquire timed out (all at max_inflight).
    #[error("{0}")]
    KeyBusy(String),
    /// Fail-closed egress when `REQUIRE_OUTBOUND_PROXY` and no healthy node lease.
    #[error("{0}")]
    NoHealthyNode(String),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    InvalidUrl(String),
    /// Client-side request-shape error (e.g. structured extraction with a
    /// non-firecrawl provider). Maps to 400 ValidationError on both surfaces.
    #[error("{0}")]
    InvalidRequest(String),
    /// Structured extraction (B18) job did not reach a terminal state within
    /// the bounded in-request poll window (min(request_timeout, 90s)).
    #[error("{0}")]
    ExtractTimeout(String),
    #[error(transparent)]
    Db(#[from] serpotter_db::DbError),
}

#[derive(Debug, Error)]
pub enum ResearchError {
    #[error(transparent)]
    Search(#[from] SearchExecError),
    #[error(transparent)]
    Extract(#[from] ExtractError),
}
