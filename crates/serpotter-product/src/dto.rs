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
    /// B26 batch extract: when present (non-empty), `url` is ignored and every
    /// entry is extracted in one vendor call. Supported backends: tavily
    /// (`provider=tavily`/auto) or exa (`provider=exa`). Batch responses
    /// arrive in `pages` (additive field); the top-level `url`/`content`
    /// carry the first page for wire compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urls: Option<Vec<String>>,
    /// B27 extraction mode: `question` (firecrawl, single URL) or `highlights`
    /// (exa, single URL). `markdown`/`text` force Tavily's `/extract` format.
    /// Absent = plain scrape/chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// B27 question extraction: the question to answer from the (single) URL.
    /// Requires `format=question` (firecrawl backend).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// B28 structured output: JSON schema the extraction must conform to.
    /// Alias of `schema` on the extract surface (firecrawl structured path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResponse {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    pub provider_used: String,
    /// Structured-extraction result (B18): the completed Firecrawl `/v2/extract`
    /// JSON object. Absent for the plain scrape path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    /// B26 batch extract: one brief per successfully extracted URL. Absent for
    /// single-URL extracts (which keep the top-level `url`/`content`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<Vec<ExtractedPageBrief>>,
}

/// One extracted page of a B26 batch extract (`{url, content}`).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedPageBrief {
    pub url: String,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
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
    /// B17: research backend — `serpotter` (default; multi-leg web+scrape+
    /// social / deep loop) or `tavily` (single Tavily `/research` job polled
    /// synchronously, answer + citations in `evidence`/`citations`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_backend: Option<String>,
    /// B31: citation format for the Tavily `/research` backend
    /// (`numbered`/`mla`/`apa`/`chicago`; absent = vendor default). Cosmetic
    /// for the serpotter backend (citations already exist — not reformatted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation_format: Option<String>,
    /// B28 structured output: JSON schema the synthesized answer should
    /// conform to. Best-effort: the deep-research xAI synthesis uses
    /// `complete_structured`; standard research leaves existing answers as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
}

/// Live wire matches mysearch ResearchResult camelCase (encodeKeys not applied at HTTP).
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResearchResponse {
    pub query: String,
    pub web_results: Vec<SearchItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_results: Option<Vec<SearchItem>>,
    /// Soft-empty social leg detail (xAI/key failure); omitted when social skipped or ok.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scraped_pages: Option<Vec<ScrapedPage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<Citation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
    /// B32 structured deep-research synthesis (fixed-shape answer object:
    /// `answer`/`reasoning`/`citations`). Present only on the deep path when
    /// the xAI synthesis succeeded; standard research leaves it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<Synthesis>,
}

/// B32 structured synthesis: the deep-research answer as a fixed-shape JSON
/// object. `answer` is always present; `reasoning` and `citations` are
/// optional and omitted when the model did not produce them (never
/// fabricated). `citations` holds 1-based indices into
/// [`ResearchResponse::citations`] (the `[n]` markers in the answer text).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Synthesis {
    pub answer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<usize>>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScrapedPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers_consulted: Option<Vec<String>>,
    /// Soft-merge web multi-leg detail when hybrid/blend kept items but a leg failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_leg_errors: Option<Vec<String>>,
}
