use rmcp::schemars;
use serde::Deserialize;
use serpotter_core::SearchQuery;
use serpotter_core::{
    normalize_choice, normalize_search_depth, normalize_sources, normalize_time_range,
    validate_deep_research_knobs, VALID_CITATION_FORMATS, VALID_EXTRACT_FORMATS,
    VALID_EXTRACT_PROVIDERS, VALID_INTENTS, VALID_MODES, VALID_PROVIDERS, VALID_RESEARCH_BACKENDS,
    VALID_STRATEGIES,
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
//
// The `normalize_*` matchers fold spelling variants first (`" Tavily "`,
// `ultra_fast`, `ULTRA-FAST` each name exactly one member) and hand back the
// canonical form. The canonical value is deliberately DISCARDED here: this
// boundary only decides accept/reject, and `SearchQuery::canonicalize()` at
// the product entry owns the single rewrite that routing, the providers and
// the B1 cache key all observe. Re-serializing at the boundary would create
// a second write path free to drift from that one.

fn validate_search_params(p: &SearchParams) -> Result<(), String> {
    normalize_choice("mode", p.mode.as_deref(), VALID_MODES)?;
    normalize_choice("intent", p.intent.as_deref(), VALID_INTENTS)?;
    normalize_choice("strategy", p.strategy.as_deref(), VALID_STRATEGIES)?;
    normalize_choice("provider", p.provider.as_deref(), VALID_PROVIDERS)?;
    // B20: search_depth accepts Tavily depths AND Exa deep modes.
    normalize_search_depth("search_depth", p.search_depth.as_deref())?;
    // time_range is now a closed set here and on REST: junk was previously
    // forwarded to the vendors unvalidated — its 400 is this wave's one
    // deliberate new refusal, replacing a silent pass-through.
    normalize_time_range("time_range", p.time_range.as_deref())?;
    // B11: sources are a closed set too — an unknown source is a client error,
    // not a silent no-op (routing would otherwise treat it as unset).
    if let Some(list) = &p.sources {
        normalize_sources("sources", &list.as_list())?;
    }
    // B9: chunks_per_source is a vendor density knob (1-3); 0+ is nonsense.
    if let Some(n) = p.chunks_per_source {
        if n == 0 {
            return Err("chunks_per_source: 0 is not a valid density (1-3)".into());
        }
    }
    Ok(())
}

/// Extract provider is a closed set (F20): firecrawl/tavily/exa implement
/// extract, and `auto` means "chain default (firecrawl first)" — same as
/// omitting the field (the product dial treats `Some("firecrawl")` and `None`
/// identically). A typo like `firecrawll` is a client error and must fail here
/// (400 ValidationError envelope) instead of surfacing as a ProviderError 502
/// from the product layer. Spelling variants of a real member (`" Firecrawl"`)
/// pass — `extract_dispatch` canonicalizes before any provider comparison, so
/// the accepted-but-differently-spelled value cannot misroute.
fn validate_extract_provider<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    match value.as_deref() {
        None | Some("") => Ok(None),
        Some("auto") => Ok(None),
        // This deserializer must decide reject-vs-keep mid-deserialize, so
        // unlike the other boundary sites it returns the ORIGINAL string (the
        // product entry re-folds it); the canonical value stays discarded.
        Some(provider) => normalize_choice("provider", Some(provider), VALID_EXTRACT_PROVIDERS)
            .map_err(serde::de::Error::custom)
            .map(|_| value),
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
    // B28: outputSchema passthrough (best-effort per provider).
    if let Some(schema) = &p.output_schema {
        v["outputSchema"] = schema.clone();
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
        description = "Preferred provider (auto, tavily, firecrawl, exa, xai, social, hybrid). Heads the routing chain; if that provider fails or refuses the request, the chain still falls back to others."
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
    #[schemars(
        description = "Search depth — Tavily depths (basic, advanced, fast, ultra-fast) or Exa deep modes (deep-lite, deep, deep-reasoning); any spelling of a listed value is accepted and dialed canonically"
    )]
    pub(crate) search_depth: Option<String>,
    #[serde(default, alias = "timeRange")]
    #[schemars(
        description = "Relative time range: day, week, month, year (single-letter aliases d/w/m/y accepted); unknown values are rejected"
    )]
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
    #[schemars(
        description = "Request raw markdown/text per result. Honored by Tavily; an xAI-only request with this set is refused (xAI returns no page content); on hybrid it applies to the web leg"
    )]
    pub(crate) include_raw_content: Option<bool>,
    #[serde(default, alias = "chunksPerSource")]
    #[schemars(
        description = "Tavily-only: snippet density 1-3 (chunks_per_source); omit for vendor default"
    )]
    pub(crate) chunks_per_source: Option<u32>,
    #[serde(default, alias = "outputSchema")]
    #[schemars(
        description = "B28: JSON schema the synthesized answer must conform to (best-effort per provider; exa deep modes synthesize server-side)"
    )]
    pub(crate) output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ExtractParams {
    #[serde(default)]
    #[schemars(description = "URL to extract (single-URL modes; omit only for batch `urls`)")]
    pub(crate) url: Option<String>,
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
    #[serde(default)]
    #[schemars(
        description = "B26 batch extract: list of URLs (tavily/exa batch backends; single `url` ignored when set)"
    )]
    pub(crate) urls: Option<McpStringList>,
    #[serde(default)]
    #[schemars(
        description = "B27 extraction mode: question (firecrawl), highlights (exa), or markdown/text (tavily extract format); case-insensitive"
    )]
    pub(crate) format: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "B27 question to answer from a single URL (requires format=question)"
    )]
    pub(crate) question: Option<String>,
    #[serde(default, alias = "outputSchema")]
    #[schemars(
        description = "B28 structured output: JSON schema the extraction must conform to (alias of schema on the extract surface)"
    )]
    pub(crate) output_schema: Option<serde_json::Value>,
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
    #[schemars(
        description = "Relative time range: day, week, month, year (single-letter aliases d/w/m/y folded); same closed set as search"
    )]
    pub(crate) time_range: Option<String>,
    #[serde(default)]
    #[schemars(description = "Country bias / locale hint")]
    pub(crate) country: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "B19: run the iterative deep-research loop (2-pass search, scrape, xAI synthesis)"
    )]
    pub(crate) deep: Option<bool>,
    #[serde(default, alias = "researchBackend")]
    #[schemars(description = "B17: research backend — serpotter (default) or tavily")]
    pub(crate) research_backend: Option<String>,
    #[serde(default, alias = "citationFormat")]
    #[schemars(description = "B31: Tavily research citation format (numbered, mla, apa, chicago)")]
    pub(crate) citation_format: Option<String>,
    #[serde(default, alias = "outputSchema")]
    #[schemars(
        description = "B28: JSON schema the synthesized answer must conform to (deep-research synthesis)"
    )]
    pub(crate) output_schema: Option<serde_json::Value>,
}

/// B17/B31: validate the new research-backend surface (closed sets), plus the
/// two shapes the product entry would otherwise silently drop.
/// Spelling-tolerant like every other boundary; the canonical rewrite is
/// `research_inner`'s job at the product entry, so `"Tavily"` cannot pick a
/// different backend than `"tavily"` in the `== "tavily"` dispatch.
pub(crate) fn validate_research_params(p: &ResearchParams) -> Result<(), String> {
    // The member sets and the deep-combo rule all come from core, in the SAME
    // order REST's `validate_research_body` uses (see `product/extract.rs`):
    // closed sets first, then `time_range`, then the deep rule. Order is part
    // of the contract — a body that trips two rules must produce the same
    // `detail`/`message` on both surfaces, and the existing pins assert the
    // closed-set text wins (e.g. `citation_format: "bogus"` must not be
    // reported as "deep: citationFormat …").
    normalize_choice(
        "research_backend",
        p.research_backend.as_deref(),
        VALID_RESEARCH_BACKENDS,
    )?;
    normalize_choice(
        "citation_format",
        p.citation_format.as_deref(),
        VALID_CITATION_FORMATS,
    )?;
    // `time_range` was search's one closed set that research forwarded raw,
    // so the same bytes meant one request on one surface and two upstream
    // bodies on the other. Same matcher, same members, same message.
    normalize_time_range("time_range", p.time_range.as_deref())?;
    validate_deep_research_knobs(
        p.deep.unwrap_or(false),
        p.research_backend.as_deref(),
        p.citation_format.as_deref(),
        p.social_max_results,
    )
}

/// B26/B27: validate the new extract surface (url/urls shape, format closed
/// set).
pub(crate) fn validate_extract_params(p: &ExtractParams) -> Result<(), String> {
    // Same `has_batch` gate REST uses (`product/extract.rs`): a batch request
    // legitimately carries no single `url`, so requiring one here made the
    // B26 batch mode unreachable over MCP (and the generated schema marked
    // `url` REQUIRED, telling clients the same thing). A single-URL request
    // with neither `url` nor `urls` still fails here, with the same
    // "missing url" text the product dispatch refuses with.
    let has_batch = p.urls.as_ref().is_some_and(|u| !u.as_list().is_empty());
    if p.url.as_deref().is_none_or(|u| u.trim().is_empty()) && !has_batch {
        return Err("missing url (set `url`, or `urls` for a batch extract)".into());
    }
    // `format` is a closed set like every other routing knob, and it used to
    // be the ONE case-sensitive member on the whole wire: `"MARKDOWN"` was a
    // 400 here and would have been a 400 again in the product dispatch. It
    // now goes through the same matcher as `provider`/`mode`, reading the set
    // from core so the two gates cannot drift.
    normalize_choice("format", p.format.as_deref(), VALID_EXTRACT_FORMATS)?;
    if p.question.as_deref().is_some_and(|q| q.trim().is_empty()) {
        return Err("question: must not be blank".into());
    }
    if let Some(urls) = &p.urls {
        if urls.as_list().is_empty() {
            return Err("urls: must not be an empty list".into());
        }
    }
    Ok(())
}

/// Build the product [`serpotter_product::ExtractRequest`] from MCP params —
/// the seam `mcp/mod.rs` uses to wire the new extract surface (B26/B27/B28).
pub(crate) fn extract_params_to_request(
    p: ExtractParams,
) -> Result<serpotter_product::ExtractRequest, String> {
    validate_extract_params(&p)?;
    Ok(serpotter_product::ExtractRequest {
        // The batch arm carries no single URL: the product DTO keeps `url` a
        // plain String only because the single-URL legs read it, and every
        // batch-capable path ignores it (`extract_dispatch` branches on
        // `urls` before the first `url` read). Validation above has already
        // guaranteed one of the two is present.
        url: p.url.unwrap_or_default(),
        provider: p.provider,
        prompt: p.prompt,
        schema: p.schema,
        urls: p.urls.map(|u| u.as_list()),
        format: p.format,
        question: p.question,
        output_schema: p.output_schema,
    })
}

/// Build the product [`serpotter_product::ResearchRequest`] from MCP params —
/// the seam `mcp/mod.rs` uses to wire research_backend/citation_format/output_schema.
pub(crate) fn research_params_to_request(
    p: ResearchParams,
) -> Result<serpotter_product::ResearchRequest, String> {
    validate_research_params(&p)?;
    Ok(serpotter_product::ResearchRequest {
        query: p.query,
        web_max_results: p.web_max_results,
        scrape_top_n: p.scrape_top_n,
        include_content: p.include_content,
        social_max_results: p.social_max_results,
        include_domains: mcp_list_to_vec_or_one(p.include_domains),
        exclude_domains: mcp_list_to_vec_or_one(p.exclude_domains),
        allowed_x_handles: mcp_list_to_vec_or_one(p.allowed_x_handles),
        excluded_x_handles: mcp_list_to_vec_or_one(p.excluded_x_handles),
        from_date: p.from_date,
        to_date: p.to_date,
        time_range: p.time_range,
        country: p.country,
        deep: p.deep.unwrap_or(false),
        research_backend: p.research_backend,
        citation_format: p.citation_format,
        output_schema: p.output_schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(value: serde_json::Value) -> Result<ExtractParams, String> {
        serde_json::from_value::<ExtractParams>(value).map_err(|e| e.to_string())
    }

    #[test]
    fn extract_provider_typo_rejected_at_boundary() {
        // Genuinely unknown members stay refused with the pre-wave message
        // shape. (`"Firecrawl"`/`"tavily "` used to be in this list: they are
        // spellings of real members, so the wave accepts them — pinned in
        // `extract_provider_accepts_equivalent_spellings` below.)
        for bad in ["firecrawll", "hybrid", "social"] {
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
    fn extract_provider_accepts_equivalent_spellings() {
        for spelling in ["Firecrawl", "tavily ", " Exa"] {
            let p = extract(serde_json::json!({
                "url": "https://example.com",
                "provider": spelling,
            }))
            .unwrap_or_else(|e| panic!("provider {spelling:?} must pass the boundary: {e}"));
            // The boundary keeps the client's bytes: `extract_dispatch` owns
            // the canonical rewrite (one write path for chain pick + cache key).
            assert_eq!(p.provider.as_deref(), Some(spelling));
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

    #[test]
    fn search_accepts_equivalent_spellings_of_valid_knobs() {
        // The wave's core acceptance on the MCP side: every one of these
        // previously came back as a 400 `ValidationError` envelope even
        // though each value names exactly one advertised member. The query
        // keeps the client's bytes — `SearchQuery::canonicalize()` at the
        // product entry owns the rewrite feeding routing and the cache key.
        let q = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "rust",
                "provider": " Tavily ",
                "searchDepth": "ultra_fast",
                "timeRange": "W",
                "strategy": "Balanced",
                "sources": [" News "],
            }))
            .unwrap(),
        )
        .expect("equivalent spellings must pass MCP validation");
        assert_eq!(q.provider.as_deref(), Some(" Tavily "));
    }

    #[test]
    fn search_rejects_unknown_time_range() {
        // The wave's ONE deliberate new refusal, stated on the boundary that
        // previously forwarded ANY time_range string to the vendors raw.
        let err = search_params_to_query(
            serde_json::from_value(serde_json::json!({
                "query": "x",
                "timeRange": "nonsense",
            }))
            .unwrap(),
        )
        .expect_err("junk time_range must fail");
        assert!(err.contains("time_range"), "{err}");
        assert!(err.contains("\"nonsense\""), "must name the value: {err}");
        assert!(
            err.contains("is not a supported value") && err.contains("week"),
            "keeps the pre-wave message shape and advertises the members: {err}"
        );
        // The advertised set itself, pinned for the schema description above.
        assert_eq!(
            serpotter_core::VALID_TIME_RANGES,
            ["day", "week", "month", "year"]
        );
    }

    #[test]
    fn research_closed_sets_accept_spellings_and_still_reject_junk() {
        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "researchBackend": " Tavily ",
            "citationFormat": "MLA",
        }))
        .expect("backend/citation spelling variants must deserialize");
        validate_research_params(&p).expect("equivalent research spellings must pass");
        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "researchBackend": "tavilyy",
        }))
        .unwrap();
        let err = validate_research_params(&p).expect_err("unknown backend must fail");
        assert!(
            err.contains("research_backend") && err.contains("\"tavilyy\""),
            "{err}"
        );
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

    // ---- T-mcp2: batch `url` gate, case-insensitive format, research parity ----

    #[test]
    fn extract_batch_needs_no_single_url() {
        // P1-2: the schema used to mark `url` REQUIRED, so the B26 batch mode
        // was unreachable over MCP. It now deserializes without `url` and
        // converts — the same `has_batch` gate REST uses.
        let p = extract(serde_json::json!({
            "urls": ["https://a.example", "https://b.example"],
        }))
        .expect("batch extract must not require `url`");
        assert!(p.url.is_none());
        let req = extract_params_to_request(p).expect("batch request must convert");
        assert_eq!(
            req.urls.unwrap(),
            vec![
                "https://a.example".to_string(),
                "https://b.example".to_string()
            ]
        );
    }

    #[test]
    fn extract_single_mode_still_requires_a_url() {
        for args in [
            serde_json::json!({}),
            serde_json::json!({ "url": "" }),
            serde_json::json!({ "url": "   " }),
            // An EMPTY batch list is not a batch request.
            serde_json::json!({ "urls": [] }),
        ] {
            let p = extract(args.clone()).unwrap_or_else(|e| panic!("{args} deserializes: {e}"));
            let err = extract_params_to_request(p).expect_err(&format!("{args} must be refused"));
            assert!(err.contains("missing url"), "{args}: {err}");
        }
    }

    #[test]
    fn extract_format_is_case_and_whitespace_tolerant() {
        // `format` was the one case-SENSITIVE knob on the wire; both gates now
        // read `VALID_EXTRACT_FORMATS` through `normalize_choice`.
        for spelling in ["MARKDOWN", " Question ", "Text", "highlights"] {
            let p = extract(serde_json::json!({
                "url": "https://example.com",
                "format": spelling,
            }))
            .expect("format spelling must deserialize");
            extract_params_to_request(p)
                .unwrap_or_else(|e| panic!("format {spelling:?} must pass: {e}"));
        }
        let p = extract(serde_json::json!({
            "url": "https://example.com",
            "format": "Markdownish",
        }))
        .unwrap();
        let err = extract_params_to_request(p).expect_err("non-member must fail");
        assert!(
            err.contains("format") && err.contains("Markdownish"),
            "must name the field and the client's bytes: {err}"
        );
        assert!(err.contains("question"), "advertises the members: {err}");
    }

    #[test]
    fn extract_urls_alias_is_not_self_referential() {
        // The deleted `alias = "urls"` was `urls` → `urls`: a no-op that read
        // like a compatibility shim. The field name itself still binds.
        let p = extract(serde_json::json!({ "urls": "https://a.example" }))
            .expect("the field name `urls` must still deserialize");
        assert_eq!(p.urls.unwrap().as_list(), vec!["https://a.example"]);
    }

    #[test]
    fn research_time_range_answers_like_search() {
        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "timeRange": "W",
        }))
        .unwrap();
        validate_research_params(&p).expect("Tavily's documented short form must pass");

        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "timeRange": "nonsense",
        }))
        .unwrap();
        let err = validate_research_params(&p).expect_err("junk must fail like search");
        assert!(
            err.contains("time_range") && err.contains("\"nonsense\""),
            "{err}"
        );
    }

    #[test]
    fn research_deep_refuses_the_knobs_its_loop_drops() {
        for (field, value) in [
            ("researchBackend", serde_json::json!("tavily")),
            ("citationFormat", serde_json::json!("mla")),
            ("socialMaxResults", serde_json::json!(5)),
        ] {
            let p: ResearchParams = serde_json::from_value(serde_json::json!({
                "query": "x",
                "deep": true,
                field: value,
            }))
            .unwrap();
            let err = validate_research_params(&p)
                .expect_err("deep + a knob the loop drops must be refused");
            assert!(err.contains("deep"), "{field}: {err}");
            assert!(
                err.contains(field),
                "must name the dropped knob: {field}: {err}"
            );
        }
    }

    #[test]
    fn research_deep_keeps_the_knobs_its_loop_honors() {
        // `socialMaxResults: 0` is "social disabled" (a no-op, not a dropped
        // request) and `scrapeTopN: 0` is honored by the deep loop's clamp, so
        // neither is refused.
        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "deep": true,
            "socialMaxResults": 0,
            "scrapeTopN": 0,
            "timeRange": "day",
        }))
        .unwrap();
        validate_research_params(&p).expect("deep + zero dials must pass");

        // The same knobs WITHOUT deep are the standard path and stay legal.
        let p: ResearchParams = serde_json::from_value(serde_json::json!({
            "query": "x",
            "researchBackend": "tavily",
            "citationFormat": "mla",
            "socialMaxResults": 5,
        }))
        .unwrap();
        validate_research_params(&p).expect("standard research keeps every knob");
    }
}
