//! Shared search types, URL normalize, RRF pipeline, and 6-gate routing.

mod pipeline;
mod routing;
mod types;
mod url_normalize;

pub use pipeline::{reciprocal_rank_fusion, RrfList};
pub use routing::{
    fallback_chain, resolve_strategy, route_search, RouteDecision, RouteInput, Strategy,
};
pub use types::{RouteDebug, SearchItem, SearchQuery, SearchResponse, Sources, VecOrOne};
pub use url_normalize::normalize_url;
