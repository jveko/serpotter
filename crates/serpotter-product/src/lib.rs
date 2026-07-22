//! Product orchestration: search, extract, research (no HTTP / auth).

mod dto;
mod error;
mod extract;
mod hold;
mod search;

pub use dto::*;
pub use error::{ExtractError, ResearchError, SearchExecError};
pub use extract::{extract_url, map_social_leg, research_inner};
pub use search::{is_exhausted_status, run_provider, search_inner};

use std::sync::Arc;

use serpotter_db::Db;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::ProviderRegistry;

#[derive(Clone)]
pub struct ProductCtx {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub outbound: Arc<ProxyPool>,
    pub providers: ProviderRegistry,
}
