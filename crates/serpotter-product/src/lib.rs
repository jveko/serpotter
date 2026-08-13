//! Product orchestration: search, extract, research (no HTTP / auth).

mod cache;
mod dto;
mod error;
mod extract;
mod hold;
mod lease;
mod meta;
mod report;
mod search;
mod ssrf;

pub use dto::*;
pub use error::{ExtractError, ResearchError, SearchExecError};
pub use extract::{
    extract_dispatch, extract_structured, extract_url, map_social_leg,
    merge_providers_consulted_real, research_inner, scraped_page_from_extract,
    select_scrape_targets,
};
pub use lease::{verdict_for, with_key_proxy, LeaseError, ReportMode};
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
    /// B1 exact-query TTL response cache: enabled flag. The API layer wires it
    /// from `CACHE_TTL_SECS` (default 300, presence of the var enables; set to
    /// `0` to disable) via [`ProductCtx::with_cache`].
    pub cache_enabled: bool,
    /// B1 cache TTL for stored responses. Default 300s.
    pub cache_ttl: std::time::Duration,
}

impl ProductCtx {
    pub fn emit(&self, event: &ProgressEvent) {
        if let Some(sink) = &self.progress {
            sink.emit(event);
        }
    }

    /// Builder-style cache config (B1). `enabled=false` disables the cache;
    /// `ttl` bounds how long a served response stays valid.
    pub fn with_cache(mut self, enabled: bool, ttl: std::time::Duration) -> Self {
        self.cache_enabled = enabled;
        self.cache_ttl = ttl;
        self
    }
}
