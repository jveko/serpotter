//! Shared search types, URL normalize, RRF pipeline, and 5-gate routing.

mod country;
mod minhash;
mod pipeline;
mod routing;
mod types;
mod url_normalize;
mod validation;

pub use country::{canonical_country_name, country_code_by_name, country_name};
pub use minhash::dedupe_near_duplicates;
pub use pipeline::{reciprocal_rank_fusion, RrfList};
pub use routing::{
    fallback_chain, resolve_strategy, route_search, RouteDecision, RouteInput, Strategy,
};
pub use types::{
    canonical_domain_list, canonical_handle_list, RouteDebug, SearchItem, SearchQuery,
    SearchResponse, Sources, VecOrOne,
};
pub use url_normalize::normalize_url;
pub use validation::{
    canonical_choice, is_deep_mode, normalize_choice, normalize_country_filter,
    normalize_domain_filter, normalize_search_depth, normalize_sources, normalize_time_range,
    split_list_field, validate_deep_research_knobs, VALID_CITATION_FORMATS, VALID_DEEP_MODES,
    VALID_EXTRACT_FORMATS, VALID_EXTRACT_PROVIDERS, VALID_INTENTS, VALID_MODES, VALID_PROVIDERS,
    VALID_RESEARCH_BACKENDS, VALID_SEARCH_DEPTHS, VALID_SOURCES, VALID_STRATEGIES,
    VALID_TIME_RANGES,
};
