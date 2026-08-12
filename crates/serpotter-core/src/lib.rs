//! Shared search types, URL normalize, RRF pipeline, and 6-gate routing.

mod pipeline;
mod routing;
mod types;
mod url_normalize;
mod validation;

pub use pipeline::{reciprocal_rank_fusion, RrfList};
pub use routing::{
    fallback_chain, resolve_strategy, route_search, RouteDecision, RouteInput, Strategy,
};
pub use types::{RouteDebug, SearchItem, SearchQuery, SearchResponse, Sources, VecOrOne};
pub use url_normalize::normalize_url;
pub use validation::{
    is_deep_mode, validate_choice, validate_search_depth, validate_sources, VALID_DEEP_MODES,
    VALID_EXTRACT_PROVIDERS, VALID_INTENTS, VALID_MODES, VALID_PROVIDERS, VALID_SEARCH_DEPTHS,
    VALID_SOURCES, VALID_STRATEGIES,
};
