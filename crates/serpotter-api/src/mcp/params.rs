use rmcp::schemars;
use serde::Deserialize;
use serpotter_core::SearchQuery;
use serpotter_core::{
    validate_choice, validate_sources, VALID_EXTRACT_PROVIDERS, VALID_INTENTS, VALID_MODES,
    VALID_PROVIDERS, VALID_SEARCH_DEPTHS, VALID_STRATEGIES,
};

// --- tool param DTOs (snake_case fields + camelCase serde aliases) ---

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum McpStringList {
    One(String),
    Many(Vec<String>),
}

impl McpStringList {
    fn into_json(self) -> serde_json::Value {
        match self {
            Self::One(s) => serde_json::Value::String(s),
            Self::Many(v) => {
                serde_json::Value::Array(v.into_iter().map(serde_json::Value::String).collect())
            }
        }
    }

    fn as_list(&self) -> Vec<String> {
        match self {
            Self::One(s) => vec![s.clone()],
            Self::Many(v) => v.clone(),
        }
    }
}

/// Map MCP list field into core `VecOrOne` via SearchQuery's camelCase serde.
fn mcp_list_field(list: Option<McpStringList>) -> Option<serde_json::Value> {
    list.map(McpStringList::into_json)
}

// --- closed-set validation for routing knobs --------------------------------
// The closed sets live in serpotter-core::validation (shared with the REST
// surface, FU10); the schemars descriptions below advertise them. Routing
// (resolve.rs / rules.rs) silently coerces unknown values (strategy -> fast,
// mode -> no-op, intent -> pass-through), so reject non-empty values outside
// the advertised sets instead of letting them mislead the client.

fn validate_search_params(p: &SearchParams) -> Result<(), String> {
    validate_choice("mode", p.mode.as_deref(), VALID_MODES)?;
    validate_choice("intent", p.intent.as_deref(), VALID_INTENTS)?;
    validate_choice("strategy", p.strategy.as_deref(), VALID_STRATEGIES)?;
    validate_choice("provider", p.provider.as_deref(), VALID_PROVIDERS)?;
    validate_choice(
        "search_depth",
        p.search_depth.as_deref(),
        VALID_SEARCH_DEPTHS,
    )?;
    // B11: sources are a closed set too — an unknown source is a client error,
    // not a silent no-op (routing would otherwise treat it as unset).
    if let Some(list) = &p.sources {
        validate_sources("sources", &list.as_list())?;
    }
    // B9: chunks_per_source is a vendor density knob (1-3); 0+ is nonsense.
    if let Some(n) = p.chunks_per_source {
        if n == 0 {
            return Err("chunks_per_source: 0 is not a valid density (1-3)".into());
        }
    }
    Ok(())
}

/// Extract provider is a closed set (F20): only firecrawl/tavily implement
/// extract, and `auto` means "chain default (firecrawl first)" — same as
/// omitting the field (the product dial treats `Some("firecrawl")` and `None`
/// identically). A typo like `firecrawll` is a client error and must fail here
/// (400 ValidationError envelope) instead of surfacing as a ProviderError 502
/// from the product layer.
fn validate_extract_provider<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some("auto") => Ok(None),
        Some(provider) => validate_choice("provider", Some(provider), VALID_EXTRACT_PROVIDERS)
            .map_err(serde::de::Error::custom)
            .map(|()| value),
    }
}

pub(crate) fn mcp_list_to_vec_or_one(
    list: Option<McpStringList>,
) -> Option<serpotter_core::VecOrOne> {
    match list {
        None => None,
        Some(McpStringList::One(s)) => Some(serpotter_core::VecOrOne::One(s)),
        Some(McpStringList::Many(v)) => Some(serpotter_core::VecOrOne::Many(v)),
    }
}

pub(crate) fn search_params_to_query(p: SearchParams) -> Result<SearchQuery, String> {
    validate_search_params(&p)?;
    let mut v = serde_json::json!({
        "query": p.query,
        "maxResults": p.max_results,
        "mode": p.mode,
        "intent": p.intent,
        "strategy": p.strategy,
        "provider": p.provider,
        "sources": p.sources.map(McpStringList::into_json),
        "includeContent": p.include_content,
        "includeDomains": mcp_list_field(p.include_domains),
        "excludeDomains": mcp_list_field(p.exclude_domains),
        "allowedXHandles": mcp_list_field(p.allowed_x_handles),
        "excludedXHandles": mcp_list_field(p.excluded_x_handles),
        "fromDate": p.from_date,
        "toDate": p.to_date,
        "searchDepth": p.search_depth,
        "timeRange": p.time_range,
        "country": p.country,
        "exactMatch": p.exact_match,
    });
    // B9 tavily-only surface: insert only when present — the core SearchQuery
    // fields are plain `bool` (serde default), so an explicit JSON `null`
    // would fail deserialization. Absent = vendor default, exactly the
    // documented semantics.
    if let Some(include_images) = p.include_images {
        v["includeImages"] = serde_json::json!(include_images);
    }
    if let Some(include_raw_content) = p.include_raw_content {
        v["includeRawContent"] = serde_json::json!(include_raw_content);
    }
    if let Some(chunks_per_source) = p.chunks_per_source {
        v["chunksPerSource"] = serde_json::json!(chunks_per_source);
    }
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchParams {
    #[schemars(description = "Search query string")]
    pub(crate) query: String,
    #[serde(default, alias = "maxResults")]
    #[schemars(description = "Max results (1–20)")]
    pub(crate) max_results: Option<u32>,
    #[serde(default)]
    #[schemars(description = "Search mode (auto, web, news, social, docs, research, github, pdf)")]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Query intent (auto, factual, status, comparison, tutorial, exploratory, news, resource)"
    )]
    pub(crate) intent: Option<String>,
    #[serde(default)]
    #[schemars(description = "Routing strategy (auto, fast, balanced, verify, deep)")]
    pub(crate) strategy: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Force a specific provider (auto, tavily, firecrawl, exa, xai, social, hybrid)"
    )]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Source filter: \"web\", \"x\"/\"social\", \"news\", \"images\", or a list of those"
    )]
    pub(crate) sources: Option<McpStringList>,
    #[serde(default, alias = "includeContent")]
    #[schemars(description = "Include full page content in results when supported")]
    pub(crate) include_content: Option<bool>,
    #[serde(default, alias = "includeDomains")]
    #[schemars(description = "Only include results from these domains (string or list)")]
    pub(crate) include_domains: Option<McpStringList>,
    #[serde(default, alias = "excludeDomains")]
    #[schemars(description = "Exclude results from these domains (string or list)")]
    pub(crate) exclude_domains: Option<McpStringList>,
    #[serde(default, alias = "allowedXHandles")]
    #[schemars(description = "X/Twitter: only these handles (string or list)")]
    pub(crate) allowed_x_handles: Option<McpStringList>,
    #[serde(default, alias = "excludedXHandles")]
    #[schemars(description = "X/Twitter: exclude these handles (string or list)")]
    pub(crate) excluded_x_handles: Option<McpStringList>,
    #[serde(default, alias = "fromDate")]
    #[schemars(description = "Lower bound date filter (YYYY-MM-DD or relative)")]
    pub(crate) from_date: Option<String>,
    #[serde(default, alias = "toDate")]
    #[schemars(description = "Upper bound date filter (YYYY-MM-DD or relative)")]
    pub(crate) to_date: Option<String>,
    #[serde(default, alias = "searchDepth")]
    #[schemars(description = "Tavily search_depth: basic, advanced, fast, ultra-fast")]
    pub(crate) search_depth: Option<String>,
    #[serde(default, alias = "timeRange")]
    #[schemars(description = "Relative time range: day, week, month, year")]
    pub(crate) time_range: Option<String>,
    #[serde(default)]
    #[schemars(description = "Country bias / locale hint for providers that support it")]
    pub(crate) country: Option<String>,
    #[serde(default, alias = "exactMatch")]
    #[schemars(description = "Prefer exact phrase matching when supported")]
    pub(crate) exact_match: Option<bool>,
    #[serde(default, alias = "includeImages")]
    #[schemars(description = "Tavily-only: request image results alongside web results")]
    pub(crate) include_images: Option<bool>,
    #[serde(default, alias = "includeRawContent")]
    #[schemars(description = "Tavily-only: request raw markdown/text for each result")]
    pub(crate) include_raw_content: Option<bool>,
    #[serde(default, alias = "chunksPerSource")]
    #[schemars(
        description = "Tavily-only: snippet density 1-3 (chunks_per_source); omit for vendor default"
    )]
    pub(crate) chunks_per_source: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExtractParams {
    #[schemars(description = "URL to extract")]
    pub(crate) url: String,
    #[serde(default, deserialize_with = "validate_extract_provider")]
    #[schemars(description = "Preferred extract provider (auto, firecrawl, tavily, exa)")]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Structured extraction (B18): natural-language instruction for what to extract; requires provider=firecrawl"
    )]
    pub(crate) prompt: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Structured extraction (B18): JSON schema the result must conform to; requires provider=firecrawl"
    )]
    pub(crate) schema: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ResearchParams {
    #[schemars(description = "Research query")]
    pub(crate) query: String,
    #[serde(
        default,
        alias = "webMaxResults",
        alias = "max_results",
        alias = "maxResults"
    )]
    #[schemars(description = "Web search result cap")]
    pub(crate) web_max_results: Option<u32>,
    #[serde(default, alias = "socialMaxResults")]
    #[schemars(description = "Social/X result cap (0 disables)")]
    pub(crate) social_max_results: Option<u32>,
    #[serde(
        default,
        alias = "scrapeTopN",
        alias = "extract_top_n",
        alias = "extractTopN"
    )]
    #[schemars(description = "How many top search hits to scrape (0–10)")]
    pub(crate) scrape_top_n: Option<u32>,
    #[serde(default, alias = "includeContent")]
    #[schemars(description = "Include full page content in scraped results when supported")]
    pub(crate) include_content: Option<bool>,
    #[serde(default, alias = "includeDomains")]
    #[schemars(description = "Only include results from these domains (string or list)")]
    pub(crate) include_domains: Option<McpStringList>,
    #[serde(default, alias = "excludeDomains")]
    #[schemars(description = "Exclude results from these domains (string or list)")]
    pub(crate) exclude_domains: Option<McpStringList>,
    #[serde(default, alias = "allowedXHandles")]
    #[schemars(description = "X/Twitter: only these handles (string or list)")]
    pub(crate) allowed_x_handles: Option<McpStringList>,
    #[serde(default, alias = "excludedXHandles")]
    #[schemars(description = "X/Twitter: exclude these handles (string or list)")]
    pub(crate) excluded_x_handles: Option<McpStringList>,
    #[serde(default, alias = "fromDate")]
    #[schemars(description = "Lower bound date filter (YYYY-MM-DD or relative)")]
    pub(crate) from_date: Option<String>,
    #[serde(default, alias = "toDate")]
    #[schemars(description = "Upper bound date filter (YYYY-MM-DD or relative)")]
    pub(crate) to_date: Option<String>,
    #[serde(default, alias = "timeRange")]
    #[schemars(description = "Relative time range: day, week, month, year")]
    pub(crate) time_range: Option<String>,
    #[serde(default)]
    #[schemars(description = "Country bias / locale hint")]
    pub(crate) country: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "B19: run the iterative deep-research loop (2-pass search, scrape, xAI synthesis)"
    )]
    pub(crate) deep: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(value: serde_json::Value) -> Result<ExtractParams, String> {
        serde_json::from_value::<ExtractParams>(value).map_err(|e| e.to_string())
    }

    #[test]
    fn extract_provider_typo_rejected_at_boundary() {
        for bad in ["firecrawll", "Firecrawl", "tavily ", "hybrid", "social"] {
            let err = extract(serde_json::json!({
                "url": "https://example.com",
                "provider": bad,
            }))
            .expect_err(&format!("provider {bad:?} must be rejected"));
            assert!(
                err.contains("provider") && err.contains("valid: auto, tavily, firecrawl, exa"),
                "error must name the field and the closed set: {err}"
            );
        }
    }

    #[test]
    fn extract_provider_closed_set_passes() {
        for ok in ["tavily", "firecrawl", "exa"] {
            let p = extract(serde_json::json!({
                "url": "https://example.com",
                "provider": ok,
            }))
            .unwrap();
            assert_eq!(p.provider.as_deref(), Some(ok));
        }
    }

    #[test]
    fn extract_provider_auto_and_missing_default_to_none() {
        // `auto` is the chain default — identical to omitting provider.
        let auto = extract(serde_json::json!({
            "url": "https://example.com",
            "provider": "auto",
        }))
        .unwrap();
        assert_eq!(auto.provider, None, "auto == unset (firecrawl-first chain)");
        let missing = extract(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert_eq!(missing.provider, None);
    }

    #[test]
    fn search_accepts_hybrid_provider() {
        let q = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "rust async",
                "provider": "hybrid",
            }))
            .unwrap(),
        )
        .expect("provider=hybrid must pass MCP validation (F21)");
        assert_eq!(q.provider.as_deref(), Some("hybrid"));
    }

    #[test]
    fn hybrid_is_in_the_shared_provider_set() {
        // E1-4 contract: the shared core set must include hybrid; without it
        // the MCP wire rejects the REST-supported dial.
        assert!(
            VALID_PROVIDERS.contains(&"hybrid"),
            "serpotter_core::validation::VALID_PROVIDERS must include hybrid (D7)"
        );
    }

    #[test]
    fn search_still_rejects_unknown_routing_values() {
        let err = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "x",
                "strategy": "bogus",
            }))
            .unwrap(),
        )
        .expect_err("unknown strategy must fail");
        assert!(err.contains("strategy"), "{err}");
    }

    // ---- B9: tavily-only search surface on the MCP wire ----

    #[test]
    fn search_accepts_tavily_surface_fields_snake_and_camel() {
        let q = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "rust",
                "include_images": true,
                "include_raw_content": true,
                "chunks_per_source": 2,
            }))
            .unwrap(),
        )
        .expect("snake_case tavily fields must pass");
        assert!(q.include_images);
        assert!(q.include_raw_content);
        assert_eq!(q.chunks_per_source, Some(2));

        let q = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "rust",
                "includeImages": true,
                "chunksPerSource": 3,
            }))
            .unwrap(),
        )
        .expect("camelCase aliases must pass");
        assert!(q.include_images);
        assert_eq!(q.chunks_per_source, Some(3));
    }

    #[test]
    fn search_rejects_zero_chunks_per_source() {
        let err = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "x",
                "chunks_per_source": 0,
            }))
            .unwrap(),
        )
        .expect_err("chunks_per_source=0 must fail");
        assert!(err.contains("chunks_per_source"), "{err}");
    }

    #[test]
    fn search_accepts_tavily_surface_defaults_when_absent() {
        let q = search_params_to_query(
            serde_json::from_value(serde_json::json!({ "query": "x" })).unwrap(),
        )
        .expect("absent tavily fields default");
        assert!(!q.include_images);
        assert!(!q.include_raw_content);
        assert_eq!(q.chunks_per_source, None);
    }

    // ---- B11: sources closed set on the MCP wire ----

    #[test]
    fn search_accepts_news_and_images_sources() {
        for src in ["web", "x", "social", "news", "images"] {
            let q = search_params_to_query(
                serde_json::from_value(serde_json::json!({
                    "query": "x",
                    "sources": src,
                }))
                .unwrap(),
            )
            .unwrap_or_else(|e| panic!("source {src} must pass: {e}"));
            assert_eq!(q.sources.as_ref().unwrap().as_list(), vec![src.to_string()]);
        }
    }

    #[test]
    fn search_rejects_unknown_source() {
        let err = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "x",
                "sources": ["banana"],
            }))
            .unwrap(),
        )
        .expect_err("banana source must fail");
        assert!(err.contains("sources"), "{err}");
        assert!(err.contains("banana"), "{err}");
    }

    // ---- B18: structured extraction params ----

    #[test]
    fn extract_accepts_prompt_and_schema() {
        let p = extract(serde_json::json!({
            "url": "https://example.com",
            "prompt": "extract the company name",
            "schema": {"type": "object", "properties": {"name": {"type": "string"}}},
        }))
        .unwrap();
        assert_eq!(p.prompt.as_deref(), Some("extract the company name"));
        assert!(p.schema.is_some());
    }

    #[test]
    fn extract_structured_fields_absent_by_default() {
        let p = extract(serde_json::json!({ "url": "https://example.com" })).unwrap();
        assert!(p.prompt.is_none());
        assert!(p.schema.is_none());
    }

    // ---- B19: deep research param ----

    #[test]
    fn research_accepts_deep_flag() {
        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "deep": true,
        }))
        .unwrap();
        assert_eq!(p.deep, Some(true));
        let p: ResearchParams =
            serde_json::from_value(serde_json::json!({ "query": "x" })).unwrap();
        assert_eq!(p.deep, None, "deep defaults to unset");
    }
}
