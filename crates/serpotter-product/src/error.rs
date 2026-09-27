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
    /// Upstream `402`: the vendor account is out of credit. Its OWN class, not
    /// a provider 502 — the whole pool is drained once every key has answered
    /// `402` (each such key is demoted by its `PaymentRequired` report, so a
    /// retry lands on a funded key only after a top-up). Retrying the same
    /// drained account cannot help, hence 503 + `retryable:false` on both
    /// surfaces.
    #[error("{0}")]
    CreditsExhausted(String),
    /// Client-side request-shape error: a parameter our own provider guard
    /// refused locally (`Unsupported`), or the pre-lease shape gate in
    /// `search_inner`. Maps to 400 ValidationError on both surfaces — the search
    /// twin of [`ExtractError::InvalidRequest`]. A refusal is only what the
    /// caller sees when no leg failed provider-side (`run_chain` /
    /// `leg_aggregate_err`), so a knob one vendor cannot express (`country` is
    /// tavily-only) is still served by the next. A *vendor-side* rejection never
    /// becomes this class: an upstream 400 stays `Provider`/502 — those have
    /// historically been OUR payload bug (Firecrawl `maxAge`, 69× Aug 27–30), and
    /// `retryable:false` would also short-circuit the fallback that rescued them.
    #[error("{0}")]
    InvalidRequest(String),
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
    /// Upstream `402`: the vendor account is out of credit. The extract twin
    /// of [`SearchExecError::CreditsExhausted`] — same 503 / `retryable:false`
    /// contract, same rationale.
    #[error("{0}")]
    CreditsExhausted(String),
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
