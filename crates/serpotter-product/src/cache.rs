//! B1 exact-query TTL response cache (fail-open).
//!
//! The cache is keyed on a deterministic canonical serialization of the FULL
//! request shape (every field that can change the provider response), hashed
//! with a dependency-free FNV-1a. Field-order independence: requests arrive as
//! JSON with arbitrary key order, but deserialization into the fixed structs
//! erases that order, so equal queries always produce the same key.
//!
//! Storage is I1's `query_cache` table via `Db::cache_get` / `Db::cache_put`
//! (same wave). Every DB error is treated as a cache miss (fail-open): a
//! broken cache never fails a request, it only costs a provider call.

use serpotter_core::{SearchQuery, VecOrOne};

use crate::dto::ResearchRequest;
use crate::ProductCtx;

/// Cache partition per product API surface (matches request_log `service`).
pub const SERVICE_SEARCH: &str = "search";
pub const SERVICE_EXTRACT: &str = "extract";
pub const SERVICE_RESEARCH: &str = "research";

/// FNV-1a 64-bit — deterministic, dependency-free. Collision risk for a
/// personal-use exact-query cache is negligible.
fn fnv1a64(input: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 16-hex key for the `query_cache.key_hash` column.
pub fn key_hash(canonical: &str) -> String {
    format!("{:016x}", fnv1a64(canonical))
}

fn list(v: &Option<VecOrOne>) -> String {
    match v {
        None => String::new(),
        Some(v) => v.as_list().join(","),
    }
}

fn auto_none(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| *s != "auto")
}

/// Deterministic canonical form of a full [`SearchQuery`]. Two requests that
/// deserialize to the same struct (any JSON key order) produce the same string.
///
/// The routing knobs `mode`/`intent`/`strategy`/`provider` are normalized the
/// same way the router treats them — `"auto"` means "unset → auto-detect"
/// (resolve.rs), so both spellings share one cache row.
pub fn canonical_query(q: &SearchQuery) -> String {
    let sources = q
        .sources
        .as_ref()
        .map(|s| s.as_list().join(","))
        .unwrap_or_default();
    let output_schema = q
        .output_schema
        .as_ref()
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    format!(
        "query={}|max_results={:?}|mode={:?}|intent={:?}|strategy={:?}|provider={:?}|sources={}|include_content={:?}|include_domains={}|exclude_domains={}|allowed_x={}|excluded_x={}|from={:?}|to={:?}|depth={:?}|time_range={:?}|country={:?}|exact={:?}|images={}|raw_content={}|chunks={:?}|output_schema={}",
        q.query,
        q.max_results,
        auto_none(&q.mode),
        auto_none(&q.intent),
        auto_none(&q.strategy),
        auto_none(&q.provider),
        sources,
        q.include_content,
        list(&q.include_domains),
        list(&q.exclude_domains),
        list(&q.allowed_x_handles),
        list(&q.excluded_x_handles),
        q.from_date,
        q.to_date,
        q.search_depth,
        q.time_range,
        q.country,
        q.exact_match,
        q.include_images,
        q.include_raw_content,
        q.chunks_per_source,
        output_schema,
    )
}

/// Deterministic canonical form of an extract request. `preferred == "auto"`
/// is normalized to `None` (the API layer does the same before dispatch), so
/// both spellings share one cache row.
pub fn canonical_extract(
    url: &str,
    preferred: Option<&str>,
    prompt: Option<&str>,
    schema: Option<&serde_json::Value>,
) -> String {
    let preferred = preferred.filter(|p| *p != "auto");
    let schema = schema
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    format!(
        "url={}|preferred={:?}|prompt={:?}|schema={}",
        url, preferred, prompt, schema
    )
}

/// Canonical form of the B26/B27 extract surface (`urls`/`format`/`question`/
/// `output_schema`). Used by the batch / question / highlights dispatch, which
/// never runs through the plain [`canonical_extract`] key.
pub fn canonical_extract_v2(
    urls: &[String],
    preferred: Option<&str>,
    format: Option<&str>,
    question: Option<&str>,
    output_schema: Option<&serde_json::Value>,
) -> String {
    let preferred = preferred.filter(|p| *p != "auto");
    let output_schema = output_schema
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    format!(
        "urls={}|preferred={:?}|format={:?}|question={:?}|output_schema={}",
        urls.join(","),
        preferred,
        format,
        question,
        output_schema
    )
}

/// Deterministic canonical form of a research request. Deep research (B19) is
/// never cached (wall-clock loops, cost variance) — callers check `deep`
/// before consulting this.
pub fn canonical_research(r: &ResearchRequest) -> String {
    let output_schema = r
        .output_schema
        .as_ref()
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    format!(
        "query={}|web_max_results={:?}|scrape_top_n={:?}|include_content={:?}|social_max_results={:?}|include_domains={}|exclude_domains={}|allowed_x={}|excluded_x={}|from={:?}|to={:?}|time_range={:?}|country={:?}|deep={}|backend={:?}|citation_format={:?}|output_schema={}",
        r.query,
        r.web_max_results,
        r.scrape_top_n,
        r.include_content,
        r.social_max_results,
        list(&r.include_domains),
        list(&r.exclude_domains),
        list(&r.allowed_x_handles),
        list(&r.excluded_x_handles),
        r.from_date,
        r.to_date,
        r.time_range,
        r.country,
        r.deep,
        r.research_backend,
        r.citation_format,
        output_schema,
    )
}

/// Look up a cached response. `None` on miss, on expiry, or on any DB error
/// (fail-open — a cache fault is a miss, never a request failure).
pub async fn cache_get(ctx: &ProductCtx, service: &str, canonical: &str) -> Option<String> {
    if !ctx.cache_enabled {
        return None;
    }
    let key = key_hash(canonical);
    ctx.db.cache_get(service, &key).await.ok().flatten()
}

/// Store a response under the ctx TTL. DB errors are ignored (fail-open).
pub async fn cache_put(ctx: &ProductCtx, service: &str, canonical: &str, response_json: &str) {
    if !ctx.cache_enabled {
        return;
    }
    let key = key_hash(canonical);
    let _ = ctx
        .db
        .cache_put(service, &key, response_json, ctx.cache_ttl.as_secs() as i64)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_core::Sources;

    #[test]
    fn key_hash_is_stable_and_distinct() {
        assert_eq!(key_hash("a"), key_hash("a"));
        assert_ne!(key_hash("a"), key_hash("b"));
        assert_eq!(key_hash(""), key_hash(""));
        assert_eq!(key_hash("a").len(), 16);
    }

    #[test]
    fn canonical_query_equal_structs_equal_strings() {
        let a = SearchQuery {
            query: "hello".into(),
            provider: Some("tavily".into()),
            sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
            include_content: Some(true),
            chunks_per_source: Some(2),
            ..Default::default()
        };
        let b = SearchQuery {
            query: "hello".into(),
            provider: Some("tavily".into()),
            sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
            include_content: Some(true),
            chunks_per_source: Some(2),
            ..Default::default()
        };
        assert_eq!(canonical_query(&a), canonical_query(&b));
    }

    #[test]
    fn canonical_query_field_order_independent_via_json() {
        // Same semantic query sent with different JSON key orders must yield the
        // same cache key (deserialization normalizes key order away).
        let json_a =
            r#"{"query":"order test","provider":"exa","maxResults":7,"includeContent":true}"#;
        let json_b =
            r#"{"includeContent":true,"maxResults":7,"provider":"exa","query":"order test"}"#;
        let a: SearchQuery = serde_json::from_str(json_a).expect("parse a");
        let b: SearchQuery = serde_json::from_str(json_b).expect("parse b");
        assert_eq!(canonical_query(&a), canonical_query(&b));
    }

    #[test]
    fn canonical_query_differing_fields_differ() {
        let base = SearchQuery {
            query: "hello".into(),
            ..Default::default()
        };
        let mut other = base.clone();
        other.max_results = Some(9);
        assert_ne!(canonical_query(&base), canonical_query(&other));
        let mut other2 = base.clone();
        other2.sources = Some(Sources::One("web".into()));
        assert_ne!(canonical_query(&base), canonical_query(&other2));
    }

    #[test]
    fn canonical_query_auto_routing_knobs_share_rows() {
        // provider="auto" / strategy="auto" route identically to unset
        // (resolve.rs treats "auto" as auto-detect) → one cache row.
        let mut auto = SearchQuery {
            query: "hello".into(),
            provider: Some("auto".into()),
            strategy: Some("auto".into()),
            ..Default::default()
        };
        let unset = SearchQuery {
            query: "hello".into(),
            ..Default::default()
        };
        assert_eq!(canonical_query(&auto), canonical_query(&unset));
        // A REAL provider still diverges.
        auto.provider = Some("exa".into());
        assert_ne!(canonical_query(&auto), canonical_query(&unset));
    }

    #[test]
    fn canonical_extract_normalizes_auto_and_schema_order() {
        // auto == None share one row; schema serialization is key-sorted
        // (serde_json default Map = BTreeMap) so key order cannot differ.
        let schema = serde_json::json!({"b": 1, "a": 2});
        assert_eq!(
            canonical_extract("https://x.example", Some("auto"), None, Some(&schema)),
            canonical_extract("https://x.example", None, None, Some(&schema))
        );
        assert_ne!(
            canonical_extract("https://x.example", Some("tavily"), None, None),
            canonical_extract("https://x.example", None, None, None)
        );
        assert_ne!(
            canonical_extract("https://x.example", None, None, None),
            canonical_extract("https://y.example", None, None, None)
        );
    }

    #[test]
    fn canonical_research_is_deterministic_and_marks_deep() {
        let a = ResearchRequest {
            query: "q".into(),
            web_max_results: Some(5),
            scrape_top_n: Some(2),
            ..Default::default()
        };
        let b = ResearchRequest {
            query: "q".into(),
            web_max_results: Some(5),
            scrape_top_n: Some(2),
            ..Default::default()
        };
        assert_eq!(canonical_research(&a), canonical_research(&b));
        let mut deep = a.clone();
        deep.deep = true;
        assert_ne!(canonical_research(&a), canonical_research(&deep));
    }
}
