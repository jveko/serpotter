//! B1 exact-query TTL response cache (fail-open).
//!
//! The cache is keyed on a deterministic canonical serialization of the FULL
//! request shape (every field that can change the provider response), SHA-256
//! hashed over `service || '\0' || canonical` — the service is part of the
//! hashed input, which is what migration 0015's "service-aware content hash"
//! DDL contract means. Field-order independence: requests arrive as JSON with
//! arbitrary key order, but deserialization into the fixed structs erases that
//! order, so equal queries always produce the same key.
//!
//! Three properties of that key are load-bearing, because a wrong key does
//! not merely miss the cache — it serves ONE CALLER ANOTHER CALLER'S response
//! with `cache_hit: true`:
//! - The digest is cryptographic (SHA-256), so no token holder can *choose* a
//!   key that addresses a row they did not write. The old FNV-1a 64-bit key
//!   was invertible: extend the input until the digest repeats a known key.
//! - The service is part of the hashed input, so two surfaces asking the same
//!   question never share a row.
//! - The canonical string is injective. List fields are framed per ELEMENT,
//!   so `urls=["a,b"]` and `urls=["a","b"]` — one entry containing a comma
//!   vs two entries — cannot render the same bytes, which a comma-join did.
//!
//! Storage is I1's `query_cache` table via `Db::cache_get` / `Db::cache_put`
//! (same wave). Every DB error is treated as a cache miss (fail-open): a
//! broken cache never fails a request, it only costs a provider call.

use serpotter_core::{SearchQuery, VecOrOne};
use sha2::{Digest, Sha256};

use crate::dto::ResearchRequest;
use crate::ProductCtx;

/// Cache partition per product API surface (the `service` value also used by
/// the request-events ring entry; the `request_log` table itself was dropped
/// in migration 0017).
pub const SERVICE_SEARCH: &str = "search";
pub const SERVICE_EXTRACT: &str = "extract";
pub const SERVICE_RESEARCH: &str = "research";

/// The `query_cache.key_hash` column, for one `(service, canonical)` pair.
///
/// SHA-256 over `service || '\0' || canonical` — the framing is what makes
/// the key service-aware, as migration 0015's DDL contract claims.
///
/// SHA-256 was chosen for preimage and collision resistance: an earlier FNV-1a
/// 64-bit key was invertible, so a token holder could extend a request's
/// canonical text until the digest repeated a known key and read that row's
/// response JSON back as their own `cache_hit: true`. Precomputing such an
/// extension is a ~2^32 search, so that property is NOT provable by a unit
/// test here and is asserted only by construction — do not mistake the tests
/// below for a proof of it; they pin determinism, service scoping, and
/// canonical injectivity, which are checkable.
///
/// The service is INSIDE the hashed input, so two surfaces minting the same
/// canonical text land on different digests and can never share a PRIMARY KEY
/// row. That is the invariant `Db::cache_put`'s `ON CONFLICT(key_hash) DO
/// UPDATE SET service = excluded.service` assumes: from this layer the
/// conflict arm can only ever re-`service` a row this same surface owns.
pub fn key_hash(service: &str, canonical: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(service.as_bytes());
    hasher.update([0u8]);
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Append one `name=value` pair to a canonical string, LENGTH-PREFIXED.
///
/// A bare `name={value}|` only works while no value is the LAST field, and
/// only because every non-terminal field is followed by a fixed continuation.
/// The declared byte length removes that dependence entirely: a reader steps
/// exactly `<len>` bytes past the value no matter what delimiters the caller
/// put inside it, which is what [`list_field`] needs to frame each entry
/// independently and what keeps `urls=["a,b"]` and `urls=["a","b"]` apart.
fn field(out: &mut String, name: &str, value: &str) {
    out.push_str(name);
    out.push('=');
    out.push_str(&value.len().to_string());
    out.push(':');
    out.push_str(value);
    out.push('|');
}

/// Append a string LIST, each entry length-prefixed — same injectivity
/// requirement as [`field`], applied per element. `urls=["a,b"]` and
/// `urls=["a","b"]` are different requests (one URL containing a comma vs two
/// URLs) and must never alias; a comma-joined list cannot say which.
///
/// The count is written first so a reader can skip a zero-element list, and
/// the entries go straight into `out` — the previous framed-then-embed shape
/// paid one scratch `String` per list field.
fn list_field(out: &mut String, name: &str, values: &[String]) {
    use std::fmt::Write as _;
    let _ = write!(out, "{name}={}:", values.len());
    for value in values {
        let _ = write!(out, "{}:{value}|", value.len());
    }
    out.push('|');
}

/// Canonical rendering of an optional knob. `{:?}` quotes and escapes, so a
/// present value and an absent one can never collapse onto the same bytes
/// (`Some("a|")` vs `None`), and `Some(3)` stays distinct from `None`.
fn dbg<T: std::fmt::Debug>(value: T) -> String {
    format!("{value:?}")
}

/// Canonical rendering of an optional JSON value (key-sorted by serde's
/// default `Map`, so field order is not a source of key drift).
fn opt_json(value: Option<&serde_json::Value>) -> String {
    value
        .and_then(|v| serde_json::to_string(v).ok())
        .unwrap_or_default()
}

/// Domain-filter list for a cache key, folded through core's ONE canonicalizer
/// instead of a second copy of the rules here.
///
/// `canonical_query` is built from an already-canonicalized `SearchQuery`, so
/// this is a no-op on the search path — but `canonical_research` keys straight
/// off a `ResearchRequest`, which is never folded in place. Without a shared
/// implementation one research request occupies a cache row per spelling
/// (`"[\"a.com\"]"` vs `["a.com"]`, `AI.COM` vs `ai.com`), and every extra
/// spelling is a guaranteed vendor-visible miss.
fn domains(v: &Option<VecOrOne>) -> Vec<String> {
    v.as_ref()
        .map(serpotter_core::canonical_domain_list)
        .unwrap_or_default()
}

/// X-handle list for a cache key. Same single-implementation guarantee as
/// [`domains`].
fn handles(v: &Option<VecOrOne>) -> Vec<String> {
    v.as_ref()
        .map(serpotter_core::canonical_handle_list)
        .unwrap_or_default()
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
    let sources = q.sources.as_ref().map(|s| s.as_list()).unwrap_or_default();
    let mut out = String::new();
    field(&mut out, "query", &q.query);
    field(&mut out, "max_results", &dbg(q.max_results));
    field(&mut out, "mode", &dbg(auto_none(&q.mode)));
    field(&mut out, "intent", &dbg(auto_none(&q.intent)));
    field(&mut out, "strategy", &dbg(auto_none(&q.strategy)));
    field(&mut out, "provider", &dbg(auto_none(&q.provider)));
    list_field(&mut out, "sources", &sources);
    field(&mut out, "include_content", &dbg(q.include_content));
    list_field(&mut out, "include_domains", &domains(&q.include_domains));
    list_field(&mut out, "exclude_domains", &domains(&q.exclude_domains));
    list_field(&mut out, "allowed_x", &handles(&q.allowed_x_handles));
    list_field(&mut out, "excluded_x", &handles(&q.excluded_x_handles));
    field(&mut out, "from", &dbg(q.from_date.as_deref()));
    field(&mut out, "to", &dbg(q.to_date.as_deref()));
    field(&mut out, "depth", &dbg(q.search_depth.as_deref()));
    field(&mut out, "time_range", &dbg(q.time_range.as_deref()));
    field(&mut out, "country", &dbg(q.country.as_deref()));
    field(&mut out, "exact", &dbg(q.exact_match));
    field(&mut out, "images", &q.include_images.to_string());
    field(&mut out, "raw_content", &q.include_raw_content.to_string());
    field(&mut out, "chunks", &dbg(q.chunks_per_source));
    field(
        &mut out,
        "output_schema",
        &opt_json(q.output_schema.as_ref()),
    );
    out
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
    let mut out = String::new();
    field(&mut out, "url", url);
    field(&mut out, "preferred", &dbg(preferred));
    field(&mut out, "prompt", &dbg(prompt));
    field(&mut out, "schema", &opt_json(schema));
    out
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
    let mut out = String::new();
    list_field(&mut out, "urls", urls);
    field(&mut out, "preferred", &dbg(preferred));
    field(&mut out, "format", &dbg(format));
    field(&mut out, "question", &dbg(question));
    field(&mut out, "output_schema", &opt_json(output_schema));
    out
}

/// Deterministic canonical form of a research request. Deep research (B19) is
/// never cached (wall-clock loops, cost variance) — callers check `deep`
/// before consulting this.
pub fn canonical_research(r: &ResearchRequest) -> String {
    let mut out = String::new();
    field(&mut out, "query", &r.query);
    field(&mut out, "web_max_results", &dbg(r.web_max_results));
    field(&mut out, "scrape_top_n", &dbg(r.scrape_top_n));
    field(&mut out, "include_content", &dbg(r.include_content));
    field(&mut out, "social_max_results", &dbg(r.social_max_results));
    list_field(&mut out, "include_domains", &domains(&r.include_domains));
    list_field(&mut out, "exclude_domains", &domains(&r.exclude_domains));
    list_field(&mut out, "allowed_x", &handles(&r.allowed_x_handles));
    list_field(&mut out, "excluded_x", &handles(&r.excluded_x_handles));
    field(&mut out, "from", &dbg(r.from_date.as_deref()));
    field(&mut out, "to", &dbg(r.to_date.as_deref()));
    field(&mut out, "time_range", &dbg(r.time_range.as_deref()));
    field(&mut out, "country", &dbg(r.country.as_deref()));
    field(&mut out, "deep", &r.deep.to_string());
    field(&mut out, "backend", &dbg(r.research_backend.as_deref()));
    field(
        &mut out,
        "citation_format",
        &dbg(r.citation_format.as_deref()),
    );
    field(
        &mut out,
        "output_schema",
        &opt_json(r.output_schema.as_ref()),
    );
    out
}

/// Look up a cached response. `None` on miss, on expiry, or on any DB error
/// (fail-open — a cache fault is a miss, never a request failure).
pub async fn cache_get(ctx: &ProductCtx, service: &str, canonical: &str) -> Option<String> {
    if !ctx.cache_enabled {
        return None;
    }
    let key = key_hash(service, canonical);
    ctx.db.cache_get(service, &key).await.ok().flatten()
}

/// Store a response under the ctx TTL. DB errors are ignored (fail-open).
pub async fn cache_put(ctx: &ProductCtx, service: &str, canonical: &str, response_json: &str) {
    if !ctx.cache_enabled {
        return;
    }
    let key = key_hash(service, canonical);
    let _ = ctx
        .db
        .cache_put(service, &key, response_json, ctx.cache_ttl.as_secs() as i64)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_core::Sources;

    /// A key two DIFFERENT requests can land on does not merely waste a cache
    /// row — it hands one caller the other's response body with
    /// `cache_hit: true`. So every input here differs in the request itself.
    #[test]
    fn key_hash_distinguishes_inputs_that_differ_in_one_byte() {
        let a = key_hash(SERVICE_SEARCH, "query=5:hello|");
        assert_ne!(
            a,
            key_hash(SERVICE_SEARCH, "query=5:hellp|"),
            "one byte of query text"
        );
        assert_ne!(
            a,
            key_hash(SERVICE_EXTRACT, "query=5:hello|"),
            "one byte of service"
        );
        assert_ne!(
            a,
            key_hash(SERVICE_SEARCH, "query=6:hello|"),
            "one byte of length frame"
        );
        assert_eq!(a.len(), 64, "sha256 hex");
    }

    /// Determinism across independently built requests: the canonical builder
    /// and the digest must agree for two separately constructed but equal
    /// `SearchQuery` values, or a repeat of one request pays for a vendor call
    /// forever.
    #[test]
    fn equal_requests_mint_one_key() {
        let build = || SearchQuery {
            query: "rust async runtime".into(),
            provider: Some("tavily".into()),
            sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
            include_content: Some(true),
            ..Default::default()
        };
        let first = build();
        let second = build();
        assert_eq!(
            canonical_query(&first),
            canonical_query(&second),
            "two equal requests, one canonical form"
        );
        assert_eq!(
            key_hash(SERVICE_SEARCH, &canonical_query(&first)),
            key_hash(SERVICE_SEARCH, &canonical_query(&second)),
            "two equal requests, one cache row"
        );
    }

    /// The service is inside the hashed input, not a separate column filter —
    /// that is what migration 0015's "service-aware content hash" contract
    /// claims, and what makes `ON CONFLICT(key_hash) DO UPDATE SET service =
    /// excluded.service` unable to re-`service` another surface's row.
    #[test]
    fn key_hash_is_service_scoped() {
        let canonical = canonical_query(&SearchQuery {
            query: "rust async runtime".into(),
            ..Default::default()
        });
        assert_ne!(
            key_hash(SERVICE_SEARCH, &canonical),
            key_hash(SERVICE_EXTRACT, &canonical),
            "same canonical text, two surfaces → two rows"
        );
        assert_ne!(
            key_hash(SERVICE_SEARCH, &canonical),
            key_hash(SERVICE_RESEARCH, &canonical),
            "three surfaces, three rows"
        );
    }

    /// The NUL framing is the other half of service scoping: without it,
    /// `service="search", canonical="\0evil"` and `service="search\0evil",
    /// canonical=""` would be the SAME hashed byte string.
    #[test]
    fn key_hash_framing_separates_service_from_canonical() {
        let a = key_hash("ab", "c");
        let b = key_hash("a", "bc");
        assert_ne!(a, b, "service/canonical boundary is unambiguous");
    }

    /// `urls=["a,b"]` (ONE entry containing a comma) and `urls=["a","b"]` (TWO
    /// entries) render identically under the old comma-join — both produced
    /// `urls=a,b` — so a batch extract of two pages was served the cached body
    /// of a single page whose URL contains a comma. Bare `a`/`b` are used
    /// deliberately: full URLs differ in more than the separator, so the old
    /// form would have told them apart and the test would prove nothing.
    #[test]
    fn extract_v2_comma_in_url_never_aliases_comma_split_list() {
        let one = vec!["a,b".to_string()];
        let two = vec!["a".to_string(), "b".to_string()];
        let one_canonical = canonical_extract_v2(&one, None, None, None, None);
        let two_canonical = canonical_extract_v2(&two, None, None, None, None);
        assert_ne!(one_canonical, two_canonical);
        assert_ne!(
            key_hash(SERVICE_EXTRACT, &one_canonical),
            key_hash(SERVICE_EXTRACT, &two_canonical),
            "one entry vs two entries must not share a row"
        );
    }

    /// The frame is what lets a reader RECOVER the field boundaries, and a
    /// value that contains the delimiters themselves is the case that
    /// distinguishes a real frame from a formatting convention. This parses
    /// the canonical bytes back with a naive `name=<len>:` scanner and checks
    /// the value round-trips exactly — no parser can do that against the old
    /// bare `name={value}|` form, which is the behavioral property (not a
    /// byte-format pin) that the length frame buys.
    ///
    /// Mutation-checked: reverting `field` to the unquoted form fails this.
    #[test]
    fn length_frame_lets_a_reader_recover_a_delimiter_bearing_value() {
        for tricky in [
            "x|prompt=Some(\"a\")|schema=:",
            "a|b",
            "https://x.example/p?q=1&r=2",
            "",
        ] {
            let canonical = canonical_extract(tricky, None, None, None);
            let value = read_framed_field(&canonical, "url").expect("url field present");
            assert_eq!(value, tricky, "framed value must round-trip: {canonical}");
        }
    }

    /// Naive reader for the `name=<len>:<value>|` frame: it trusts the
    /// declared length, so it is exactly the reader the frame is written for.
    fn read_framed_field<'a>(canonical: &'a str, name: &str) -> Option<&'a str> {
        // `canonical` starts at the frame's `name`; `rest` starts after it, so
        // every offset below is relative to `rest`, not to `canonical`.
        let rest = canonical.strip_prefix(name)?.strip_prefix('=')?;
        let colon = rest.find(':')?;
        let len: usize = rest[..colon].parse().ok()?;
        rest.get(colon + 1..colon + 1 + len)
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

    /// The research key is built straight off a `ResearchRequest`, which is
    /// never folded in place like a `SearchQuery` is — so it must fold through
    /// core's shared canonicalizer, or every client spelling of one filter set
    /// claims its own cache row and pays for a vendor call.
    #[test]
    fn canonical_research_folds_domain_and_handle_spellings() {
        let blob = ResearchRequest {
            query: "q".into(),
            include_domains: Some(VecOrOne::One(r#"["AI.Meta.COM" , "dev.meta.ai"]"#.into())),
            allowed_x_handles: Some(VecOrOne::One("@Foo".into())),
            ..Default::default()
        };
        let clean = ResearchRequest {
            query: "q".into(),
            include_domains: Some(VecOrOne::Many(vec![
                "ai.meta.com".into(),
                "dev.meta.ai".into(),
            ])),
            allowed_x_handles: Some(VecOrOne::One("foo".into())),
            ..Default::default()
        };
        assert_eq!(
            canonical_research(&blob),
            canonical_research(&clean),
            "one filter set, one row"
        );
        // Distinct sets must still key apart (the fold is not a collapse).
        let mut other = clean.clone();
        other.include_domains = Some(VecOrOne::Many(vec!["example.com".into()]));
        assert_ne!(canonical_research(&clean), canonical_research(&other));
    }

    // --- end-to-end: the real Db, the real upsert ----------------------------

    async fn cache_test_ctx() -> ProductCtx {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate");
        let pinned = "http://127.0.0.1:9";
        ProductCtx {
            keys: std::sync::Arc::new(serpotter_keypool::KeyPool::new(db.clone())),
            outbound: std::sync::Arc::new(serpotter_outbound::ProxyPool::new(db.clone())),
            providers: serpotter_providers::ProviderRegistry::with_clients(
                serpotter_providers::TavilyClient::new(pinned),
                serpotter_providers::FirecrawlClient::new(pinned),
                serpotter_providers::ExaClient::new(pinned),
                serpotter_providers::XaiClient::new(pinned),
            ),
            progress: None,
            meta_sink: None,
            request_timeout: std::time::Duration::from_secs(1),
            cache_enabled: true,
            cache_ttl: std::time::Duration::from_secs(300),
            db,
        }
    }

    /// The full round trip across two surfaces: search writes a row, extract
    /// asks for the same query text and must MISS. Under the old key (no
    /// service in the hashed input) the extract read the search row and
    /// answered with the wrong DTO's JSON.
    #[tokio::test]
    async fn cross_surface_write_never_reads_the_other_surface_row() {
        let ctx = cache_test_ctx().await;
        let canonical = canonical_query(&SearchQuery {
            query: "shared text".into(),
            ..Default::default()
        });
        cache_put(&ctx, SERVICE_SEARCH, &canonical, r#"{"surface":"search"}"#).await;
        assert_eq!(
            cache_get(&ctx, SERVICE_SEARCH, &canonical).await.as_deref(),
            Some(r#"{"surface":"search"}"#)
        );
        assert_eq!(
            cache_get(&ctx, SERVICE_EXTRACT, &canonical).await,
            None,
            "extract must not read the search row"
        );
        cache_put(
            &ctx,
            SERVICE_EXTRACT,
            &canonical,
            r#"{"surface":"extract"}"#,
        )
        .await;
        assert_eq!(
            cache_get(&ctx, SERVICE_SEARCH, &canonical).await.as_deref(),
            Some(r#"{"surface":"search"}"#),
            "extract's write must not re-service (evict) the search row"
        );
    }

    /// The same aliasing hazard, end to end through the real table. Bare
    /// `a,b` vs `a` + `b` is used rather than full URLs because the old
    /// comma-join only collided when the joined text was identical — a
    /// realistic-looking pair of URLs differs in more than the separator and
    /// would have passed the broken version.
    #[tokio::test]
    async fn comma_url_and_comma_split_urls_occupy_separate_rows() {
        let ctx = cache_test_ctx().await;
        let one = canonical_extract_v2(&["a,b".to_string()], None, None, None, None);
        let two = canonical_extract_v2(&["a".to_string(), "b".to_string()], None, None, None, None);
        cache_put(&ctx, SERVICE_EXTRACT, &one, "one-url").await;
        cache_put(&ctx, SERVICE_EXTRACT, &two, "two-urls").await;
        assert_eq!(
            cache_get(&ctx, SERVICE_EXTRACT, &one).await.as_deref(),
            Some("one-url")
        );
        assert_eq!(
            cache_get(&ctx, SERVICE_EXTRACT, &two).await.as_deref(),
            Some("two-urls"),
            "the two-URL read must not land on the one-URL row"
        );
        // Both rows survive: had the second put upserted over the first, the
        // `one` read above would have returned "two-urls".
    }
}
