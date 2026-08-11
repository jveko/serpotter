//! Wire DTOs for extract/research product paths (camelCase serde parity with API).

use serde::{Deserialize, Serialize};
use serpotter_core::SearchItem;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    pub url: String,
    /// Optional force provider: firecrawl | tavily | exa (auto → firecrawl first).
    pub provider: Option<String>,
    /// Structured extraction (B18): natural-language instruction for what to
    /// extract. When set (with or without `schema`), provider must be
    /// firecrawl (or auto → firecrawl): the only structured backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Structured extraction (B18): JSON schema the result must conform to.
    /// Provider rule identical to `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResponse {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    pub provider_used: String,
    /// Structured-extraction result (B18): the completed Firecrawl `/v2/extract`
    /// JSON object. Absent for the plain scrape path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRequest {
    pub query: String,
    /// mysearch REST: webMaxResults. Aliases: maxResults.
    #[serde(default, alias = "maxResults", alias = "max_results")]
    pub web_max_results: Option<u32>,
    /// mysearch REST/MCP: scrapeTopN / scrape_top_n. Aliases: extractTopN.
    #[serde(
        default,
        alias = "extractTopN",
        alias = "extract_top_n",
        alias = "scrape_top_n"
    )]
    pub scrape_top_n: Option<u32>,
    pub include_content: Option<bool>,
    /// mysearch: socialMaxResults (0 = skip social).
    #[serde(default, alias = "social_max_results")]
    pub social_max_results: Option<u32>,
    #[serde(default, alias = "include_domains")]
    pub include_domains: Option<serpotter_core::VecOrOne>,
    #[serde(default, alias = "exclude_domains")]
    pub exclude_domains: Option<serpotter_core::VecOrOne>,
    #[serde(default, alias = "allowed_x_handles")]
    pub allowed_x_handles: Option<serpotter_core::VecOrOne>,
    #[serde(default, alias = "excluded_x_handles")]
    pub excluded_x_handles: Option<serpotter_core::VecOrOne>,
    #[serde(default, alias = "from_date")]
    pub from_date: Option<String>,
    #[serde(default, alias = "to_date")]
    pub to_date: Option<String>,
    #[serde(default, alias = "time_range")]
    pub time_range: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
    /// B19: run the iterative deep-research loop (2-pass search → scrape →
    /// xAI synthesis, capped by the request deadline). `false` = classic
    /// research (web + scrape + optional social).
    #[serde(default)]
    pub deep: bool,
}

/// Live wire matches mysearch ResearchResult camelCase (encodeKeys not applied at HTTP).
#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResearchResponse {
    pub query: String,
    pub web_results: Vec<SearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub social_results: Option<Vec<SearchItem>>,
    /// Soft-empty social leg detail (xAI/key failure); omitted when social skipped or ok.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub social_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scraped_pages: Option<Vec<ScrapedPage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<Citation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
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

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers_consulted: Option<Vec<String>>,
    /// Soft-merge web multi-leg detail when hybrid/blend kept items but a leg failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_leg_errors: Option<Vec<String>>,
}
