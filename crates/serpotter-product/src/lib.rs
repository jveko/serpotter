//! Product orchestration: search, extract, research (no HTTP / auth).

mod dto;
mod error;
mod extract;
mod search;

pub use dto::*;
pub use error::{ExtractError, ResearchError, SearchExecError};

use std::sync::Arc;

use serpotter_db::Db;
use serpotter_keypool::KeyPool;
use serpotter_providers::ProviderRegistry;

#[derive(Clone)]
pub struct ProductCtx {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub providers: ProviderRegistry,
}
