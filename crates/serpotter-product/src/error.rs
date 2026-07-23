//! Product-layer errors (search / extract / research). Handlers map these to problem details.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SearchExecError {
    #[error("{0}")]
    NoHealthyKey(String),
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
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    InvalidUrl(String),
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
