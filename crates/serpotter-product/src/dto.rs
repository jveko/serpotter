//! Wire DTOs for extract/research product paths (camelCase serde parity with API).

use serde::{Deserialize, Serialize};
use serpotter_core::SearchItem;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    pub url: String,
    /// Optional force provider: firecrawl | tavily
    pub provider: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResponse {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    pub provider_used: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRequest {
    pub query: String,
    /// mysearch REST: webMaxResults. Aliases: maxResults.
    #[serde(default, alias = "maxResults", alias = "max_results")]
    pub web_max_results: Option<u32>,
    /// mysearch REST/MCP: scrapeTopN / scrape_top_n. Aliases: extractTopN.
    #[serde(default, alias = "extractTopN", alias = "extract_top_n", alias = "scrape_top_n")]
    pub scrape_top_n: Option<u32>,
    pub include_content: Option<bool>,
    /// mysearch: socialMaxResults (0 = skip social).
    #[serde(default, alias = "social_max_results")]
    pub social_max_results: Option<u32>,
}

/// Live wire matches mysearch ResearchResult camelCase (encodeKeys not applied at HTTP).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchResponse {
    pub query: String,
    pub web_results: Vec<SearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub social_results: Option<Vec<SearchItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scraped_pages: Option<Vec<ScrapedPage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<Citation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapedPage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers_consulted: Option<Vec<String>>,
}
