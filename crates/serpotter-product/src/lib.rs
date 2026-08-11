//! Product orchestration: search, extract, research (no HTTP / auth).

mod dto;
mod error;
mod extract;
mod hold;
mod meta;
mod report;
mod search;
mod ssrf;

pub use dto::*;
pub use error::{ExtractError, ResearchError, SearchExecError};
pub use extract::{
    extract_url, map_social_leg, merge_providers_consulted_real, research_inner,
    scraped_page_from_extract, select_scrape_targets,
};
pub use meta::{ExecMeta, NoopSink, ProductOutcome, ProgressEvent, ProgressSink};
pub use report::{classify_proxied_http, ProxiedHttpClass};
pub use search::{
    first_blend_err, is_exhausted_status, is_firecrawl_banned, multi_leg_errors, run_provider,
    search_inner,
};

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
    /// Outbound progress observer (MCP sets it; REST leaves `None`).
    pub progress: Option<Arc<dyn ProgressSink>>,
    /// Overall per-request deadline (F10). The API layer wraps every product
    /// call in `tokio::time::timeout` with this budget and answers 504 /
    /// MCP `Timeout` when it elapses. Wired from `REQUEST_TIMEOUT_SECS`
    /// (default 120s) by `AppState::product_ctx`.
    pub request_timeout: std::time::Duration,
}

impl ProductCtx {
    pub fn emit(&self, event: &ProgressEvent) {
        if let Some(sink) = &self.progress {
            sink.emit(event);
        }
    }
}
