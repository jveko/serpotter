//! Extract / research orchestration. No HTTP / auth.

mod extract_url;
mod helpers;
mod research;

pub use extract_url::extract_url;
pub use helpers::{
    map_social_leg, merge_providers_consulted, scraped_page_from_extract, select_scrape_targets,
};
pub use research::research_inner;
