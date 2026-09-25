use serde::{Deserialize, Serialize};

use crate::validation::{
    normalize_choice, normalize_domain_filter, normalize_search_depth, normalize_sources,
    normalize_time_range, split_list_field, VALID_INTENTS, VALID_MODES, VALID_PROVIDERS,
    VALID_STRATEGIES,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Relevance score. On multi-leg merged output (hybrid/blend via RRF) this
    /// is the fused reciprocal-rank-fusion score, consistent with the final
    /// ordering. Single-chain results keep the raw provider score (or None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub query: String,
    pub provider_used: String,
    pub items: Vec<SearchItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Soft-merge detail when hybrid (or multi-leg) keeps results but a leg failed.
    /// Omitted when all contributing legs succeeded or both legs empty (hard error path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leg_errors: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_debug: Option<RouteDebug>,
    /// B1: set to `true` when the response was served from the exact-query TTL
    /// cache (no provider call happened for this request). Absent on normal
    /// execution — additive wire field, never persisted into the cache row
    /// itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_hit: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteDebug {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request shape for `/api/search`. `rename_all = "camelCase"` is the documented
/// REST wire form, but every multi-word field also carries its `snake_case`
/// spelling as an alias: the MCP surface has always accepted both (its params
/// structs alias the camelCase twin), so before this a client that sent
/// `time_range`/`search_depth` to REST got the field silently DROPPED — an
/// unfiltered search that still returned 200. Aliasing makes the two surfaces
/// accept the same inputs instead of failing differently. Purely additive: the
/// serialized form stays camelCase everywhere.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub query: String,
    #[serde(alias = "max_results")]
    pub max_results: Option<u32>,
    pub mode: Option<String>,
    pub intent: Option<String>,
    pub strategy: Option<String>,
    pub provider: Option<String>,
    pub sources: Option<Sources>,
    #[serde(alias = "include_content")]
    pub include_content: Option<bool>,
    #[serde(alias = "include_domains")]
    pub include_domains: Option<VecOrOne>,
    #[serde(alias = "exclude_domains")]
    pub exclude_domains: Option<VecOrOne>,
    #[serde(alias = "allowed_x_handles")]
    pub allowed_x_handles: Option<VecOrOne>,
    #[serde(alias = "excluded_x_handles")]
    pub excluded_x_handles: Option<VecOrOne>,
    #[serde(alias = "from_date")]
    pub from_date: Option<String>,
    #[serde(alias = "to_date")]
    pub to_date: Option<String>,
    #[serde(alias = "search_depth")]
    pub search_depth: Option<String>,
    #[serde(alias = "time_range")]
    pub time_range: Option<String>,
    pub country: Option<String>,
    #[serde(alias = "exact_match")]
    pub exact_match: Option<bool>,
    /// Tavily-only: request image results (Tavily `/search` `include_images`).
    /// Other providers ignore it.
    #[serde(default, alias = "include_images")]
    pub include_images: bool,
    /// Request raw markdown/text for each result. Honored by Tavily
    /// (`include_raw_content`); an xAI-only request with this set is refused
    /// (400 Unsupported) because xAI results carry no page content, and the x
    /// leg of a hybrid request has it stripped — the web leg still honors it.
    #[serde(default, alias = "include_raw_content")]
    pub include_raw_content: bool,
    /// Tavily-only: snippet density 1-3 (Tavily `chunks_per_source`); `None` =
    /// vendor default. Other providers ignore it.
    #[serde(
        default,
        alias = "chunks_per_source",
        skip_serializing_if = "Option::is_none"
    )]
    pub chunks_per_source: Option<u32>,
    /// B28 structured output: JSON schema the synthesized answer must conform
    /// to. Best-effort per provider: Exa `/search` deep modes carry it as
    /// `outputSchema` (server-side structured synthesis), xAI research
    /// synthesis uses `complete_structured`; web-only providers ignore it and
    /// the search items stay as-is. Absent = unconstrained answer.
    #[serde(
        default,
        alias = "output_schema",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Sources {
    One(String),
    Many(Vec<String>),
}

impl Sources {
    pub fn as_list(&self) -> Vec<String> {
        match self {
            Sources::One(s) => vec![s.clone()],
            Sources::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VecOrOne {
    One(String),
    Many(Vec<String>),
}

impl VecOrOne {
    /// Entries as a list. A `One` string is coerced with
    /// [`crate::validation::split_list_field`] so a client's stringified JSON
    /// array (`"[\"a.com\",\"b.com\"]"`) becomes two real filter entries
    /// instead of one bogus domain the vendor answers with a 400. `Many`
    /// entries keep their text but lose empty ones, so `is_nonempty` and
    /// `as_list` can never disagree about whether a filter is set.
    pub fn as_list(&self) -> Vec<String> {
        match self {
            VecOrOne::One(s) => crate::validation::split_list_field(s),
            VecOrOne::Many(v) => v.iter().filter(|s| !s.trim().is_empty()).cloned().collect(),
        }
    }

    /// True when [`Self::as_list`] yields at least one entry (a whitespace-only
    /// or stringified-empty-array `One` counts as unset).
    pub fn is_nonempty(&self) -> bool {
        !self.as_list().is_empty()
    }
}

impl SearchQuery {
    pub fn clamped_max_results(&self) -> u32 {
        self.max_results.unwrap_or(5).clamp(1, 20)
    }

    /// Fold every knob the client may have spelled differently into the single
    /// canonical form used by routing, the providers and the B1 cache key.
    ///
    /// Formatting only — never validation. A value that is not a recognized
    /// member of its closed set is left EXACTLY as the client sent it
    /// (`mode: "banana"` stays `banana`), because rejection belongs to the
    /// boundary and that boundary error must name the bytes the caller actually
    /// sent; dropping or guessing unrecognized values would hide a fixable typo
    /// behind an answer that looks like the request the caller meant. Nothing
    /// here can change what a request asks for: `www.` stays on a domain (a
    /// vendor-visible difference), an unrecognized date passes through
    /// byte-for-byte, and a domain that is not a plausible hostname keeps its
    /// text so it still draws the same refusal. The one thing folded away is a
    /// BLANK knob, which the boundary's own rule already calls unset (see
    /// `fold_member`).
    ///
    /// CONTRACT FOR CALLERS: run this first and read the body only afterwards.
    /// Routing, the provider dial, the cache key and any raw comparison in the
    /// api/product layer (`body.provider == "firecrawl"` for structured
    /// extraction, a `mode == "social"` branch, a `sources.contains("x")` test)
    /// all have to look at post-canonicalize values — otherwise a perfectly
    /// legal `"Firecrawl"` silently takes the non-canonical branch.
    pub fn canonicalize(&mut self) {
        // Providers already `.trim()` the query before dialing, so the trim
        // only aligns the cache key; collapsing internal whitespace is what
        // makes `"foo   bar"` and `"foo bar"` one search instead of two cache
        // rows and two upstream bodies (every vendor tokenizes on whitespace).
        // Case and punctuation stay: `C++`, `NASA` and `iPhone` are not the
        // same question in lowercase.
        self.query = collapse_spaces(self.query.trim());
        fold_member(&mut self.mode, "mode", VALID_MODES);
        fold_member(&mut self.intent, "intent", VALID_INTENTS);
        fold_member(&mut self.strategy, "strategy", VALID_STRATEGIES);
        fold_member(&mut self.provider, "provider", VALID_PROVIDERS);
        fold_verdict(&mut self.search_depth, |raw| {
            normalize_search_depth("search_depth", Some(raw))
        });
        fold_verdict(&mut self.time_range, |raw| {
            normalize_time_range("time_range", Some(raw))
        });
        fold_sources(&mut self.sources);
        fold_list(&mut self.include_domains, fold_domain_entry);
        fold_list(&mut self.exclude_domains, fold_domain_entry);
        fold_list(&mut self.allowed_x_handles, |entry| {
            vec![canonical_handle_entry(entry)]
        });
        fold_list(&mut self.excluded_x_handles, |entry| {
            vec![canonical_handle_entry(entry)]
        });
        fold_date(&mut self.from_date);
        fold_date(&mut self.to_date);
        // `country` is deliberately NOT folded here, unlike the other knobs:
        // Tavily's dialect is the full lowercase name while Firecrawl's is the
        // uppercase ISO-2 code, and Firecrawl documents `UK` — a spelling
        // core's table cannot prove, so it passes it through verbatim. Any
        // single "canonical" country string written here would rewrite a
        // working Firecrawl filter into a different one, so each provider keeps
        // translating the client's own value (see `normalize_country_filter`
        // and `providers/src/firecrawl.rs`).
    }
}

/// Apply the BOUNDARY's own verdict to one knob, so core can never hold two
/// answers to "what is this value":
/// * `Ok(Some(canonical))` — write the member's canonical spelling.
/// * `Ok(None)` — the boundary calls a blank knob UNSET, so the field is
///   cleared. Leaving `Some("  ")` in place would not be "untouched": routing
///   reads an explicit `Some` as a pin (`resolve_strategy`'s `_ => Fast`) and
///   the cache would otherwise key it as its own row (`auto_none` collapses
///   only `"auto"`), while the boundary calls the same value absent — one
///   request, three answers, which is the defect class this wave exists to kill.
/// * `Err(_)` — leave the client's bytes EXACTLY as they are. A value the
///   boundary refuses must survive so the boundary's message can name it
///   (`mode: "banana"` stays `banana`; so does `"."`, which folds to empty).
fn fold_member(value: &mut Option<String>, field: &str, valid: &[&str]) {
    let verdict = value
        .as_deref()
        .map(|raw| normalize_choice(field, Some(raw), valid));
    if let Some(Ok(canonical)) = verdict {
        *value = canonical;
    }
}

/// Same verdict for a knob whose accepted spellings are not a plain closed set
/// (the depth union, the `time_range` short forms): call its normalizer.
fn fold_verdict(
    value: &mut Option<String>,
    normalize: impl FnOnce(&str) -> Result<Option<String>, String>,
) {
    let verdict = value.as_deref().map(normalize);
    if let Some(Ok(canonical)) = verdict {
        *value = canonical;
    }
}

/// Fold a `sources` list entry-by-entry, reusing [`normalize_sources`] so the
/// formatter and the boundary agree on what is a source.
fn fold_sources(sources: &mut Option<Sources>) {
    let Some(current) = sources.as_ref() else {
        return;
    };
    let raw = current.as_list();
    let was_single = raw.len() == 1 && matches!(current, Sources::One(_));
    let entries: Vec<String> = raw
        .iter()
        .flat_map(|entry| split_list_field(entry.as_str()))
        .collect();
    let Ok(canonical) = normalize_sources("sources", &entries) else {
        return;
    };
    // Nothing left to fold means the client sent only blanks. Unlike a string
    // knob there is no divergence to close here: every consumer of a list field
    // reads it through `as_list()` (and `VecOrOne::is_nonempty`), which already
    // call a blank list absent, so leaving the field alone is honest and
    // inventing `Some(Many(vec![]))` is not.
    if canonical.is_empty() {
        return;
    }
    *sources = Some(if was_single && canonical.len() == 1 {
        Sources::One(canonical[0].clone())
    } else {
        Sources::Many(canonical)
    });
}

/// Canonical domain-filter entries of a client list, as one flat list.
///
/// Exported so the extract/research cache keys fold with the SAME rules as the
/// request that actually gets dialed — `SearchQuery::canonicalize()` folds in
/// place, but a research body is keyed straight from its own struct, so without
/// this the two surfaces would need two copies of the `*.`/`@`/case rules, which
/// is precisely the drift this wave keeps removing. Idempotent: folding an
/// already-canonical list is a no-op, so calling it on a canonicalized
/// `SearchQuery` is harmless and lets both keys share one code path.
pub fn canonical_domain_list(list: &VecOrOne) -> Vec<String> {
    fold_entries(list, fold_domain_entry)
}

/// Canonical X-handle entries of a client list. Same single-implementation
/// guarantee as [`canonical_domain_list`]. No whitespace split here: a handle
/// has no plausibility test to gate it with, so `"foo bar"` would be split on a
/// guess.
pub fn canonical_handle_list(list: &VecOrOne) -> Vec<String> {
    fold_entries(list, |entry| vec![canonical_handle_entry(entry)])
}

/// One client entry → the filter(s) it actually contains.
///
/// A hostname cannot contain a comma, so [`split_list_field`] splits those
/// unconditionally. A SPACE is ambiguous — "two filters typed into one string"
/// versus "one mistyped filter" — so it splits only when EVERY piece is a
/// plausible host: `"a.com b.com"` becomes two working filters, while
/// `"my site.com"` stays one entry so the local refusal still names the text the
/// client sent instead of the fragment `"my"`. Leniency without losing the
/// diagnostic.
fn fold_domain_entry(entry: &str) -> Vec<String> {
    let folded = canonical_domain_entry(entry);
    if normalize_domain_filter(&folded).is_some() {
        return vec![folded];
    }
    let pieces: Vec<String> = entry
        .split_whitespace()
        .map(canonical_domain_entry)
        .collect();
    if pieces.len() > 1 && pieces.iter().all(|p| normalize_domain_filter(p).is_some()) {
        pieces
    } else {
        vec![folded]
    }
}

/// The fold shared by the in-place `fold_list` below and the exported helpers
/// above: split each client entry through [`split_list_field`] — the single
/// coercion point for commas and stringified JSON arrays — then let `expand`
/// decide whether one entry is one filter or several.
fn fold_entries(list: &VecOrOne, expand: impl Fn(&str) -> Vec<String>) -> Vec<String> {
    list.as_list()
        .iter()
        .flat_map(|entry| split_list_field(entry.as_str()))
        .flat_map(|entry| expand(entry.as_str()))
        .collect()
}

/// Fold a one-or-many list field in place.
///
/// The client's variant survives when one entry folds to one entry; a split
/// necessarily becomes `Many`. Everything downstream reads `as_list()`, so this
/// only keeps the debug view close to the request.
fn fold_list(list: &mut Option<VecOrOne>, expand: impl Fn(&str) -> Vec<String>) {
    let Some(current) = list.as_ref() else { return };
    let raw = current.as_list();
    let was_single = raw.len() == 1 && matches!(current, VecOrOne::One(_));
    let entries = fold_entries(current, expand);
    if entries.is_empty() {
        return;
    }
    *list = Some(match (was_single, entries.len()) {
        (true, 1) => VecOrOne::One(entries[0].clone()),
        _ => VecOrOne::Many(entries),
    });
}

/// One domain-filter entry → the bare hostname a vendor accepts.
fn canonical_domain_entry(entry: &str) -> String {
    // A leading `*.` is noise, not a different filter: a domain entry already
    // matches the whole domain, so `*.ai.meta.com` asks for exactly what
    // `ai.meta.com` asks for. Only that prefix is removed — an inner `*`
    // (`foo.*.com`) is no vendor's syntax and keeps its refusal.
    let starless = entry.strip_prefix("*.").unwrap_or(entry);
    // Every other rule stays `normalize_domain_filter`'s: lowercase, trim,
    // scheme/userinfo/path/port/quotes off, `www.` kept, implausible hosts
    // refused. A refused entry comes back with the client's OWN text — not the
    // wildcard-stripped form, which would rewrite `"*."` into nothing — so the
    // existing local refusal still names what was sent.
    normalize_domain_filter(starless).unwrap_or_else(|| entry.trim().to_string())
}

/// One X handle → the bare handle the wire expects.
fn canonical_handle_entry(entry: &str) -> String {
    let trimmed = entry.trim();
    // EXACTLY one leading `@`: that is the paste-a-profile shape (`@elonmusk`).
    // A second `@` is not a marker we invent a meaning for, and an entry that
    // is nothing but `@` stays as it is rather than becoming a blank filter.
    let bare = trimmed.strip_prefix('@').unwrap_or(trimmed);
    if bare.is_empty() {
        return trimmed.to_string();
    }
    bare.to_lowercase()
}

/// Canonicalize one date knob in place: rewrite only what
/// [`canonical_civil_date`] recognizes, leave everything else exactly as sent.
fn fold_date(value: &mut Option<String>) {
    // Compute first, then write: nothing borrows `value` across the update, the
    // same shape `fold_member`/`fold_verdict` use.
    let rewritten = value.as_deref().and_then(canonical_civil_date);
    if rewritten.is_some() {
        *value = rewritten;
    }
}

/// Rewrite the two unambiguous civil-date spellings — `2026-9-1` and
/// `2026/9/1` — into the `YYYY-MM-DD` the providers actually parse, and
/// nothing else.
///
/// Tavily takes `start_date`/`end_date` as civil dates, and Firecrawl's
/// `ymd_to_us_mdy` parser returns `None` unless the year is four digits, so
/// today a client's unpadded date is silently dropped (an unbounded search the
/// caller believes it filtered) or 400s upstream. Padding is the only safe
/// rewrite: same year, same month, same day, one ASCII shape.
///
/// Anything unrecognized passes through BYTE-FOR-BYTE — an ISO datetime, an
/// epoch, `last tuesday`, a two-digit year, a bogus month — because the existing
/// error paths and the client's own value are worth more than a guess, and
/// reordering a date would silently change which window the caller asked for.
fn canonical_civil_date(raw: &str) -> Option<String> {
    let value = raw.trim();
    // One separator style only, and exactly three parts: `2026-09-01T00:00:00Z`
    // fails the part count, `2026/9-1` fails the single-style rule, and a value
    // with no separator at all is never reformatted.
    let separator = match (value.contains('-'), value.contains('/')) {
        (true, false) => '-',
        (false, true) => '/',
        _ => return None,
    };
    let mut parts = value.split(separator);
    let (Some(year), Some(month), Some(day), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    // Digits only: a `+` sign, spaces or a weekday name are not ours to parse.
    let is_digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !is_digits(year) || !is_digits(month) || !is_digits(day) {
        return None;
    }
    // Four-digit year (the vendors' own requirement), and no over-long parts:
    // `2026-009-1` is a typo, not a padding job.
    if year.len() != 4 || month.len() > 2 || day.len() > 2 {
        return None;
    }
    let month: u32 = month.parse().ok()?;
    let day: u32 = day.parse().ok()?;
    // Month 01-12 and day 01-31 — no per-month length and no leap-year rule:
    // `2026-02-30` is the vendor's refusal today, and a stricter local guess
    // here would only invent a new error for a value we do not rewrite anyway.
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(format!("{year}-{:02}-{day:02}", month))
}

/// Collapse runs of Unicode whitespace into one space (query text: whitespace
/// is a token separator there and carries no other meaning).
fn collapse_spaces(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending = false;
    for c in value.chars() {
        if c.is_whitespace() {
            pending = !out.is_empty();
            continue;
        }
        if pending {
            out.push(' ');
        }
        pending = false;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_coerces_a_stringified_json_array() {
        // The exact live shape that cost us a retryable 502: agents stringify
        // the array, and it must not reach a vendor as one bogus domain.
        assert_eq!(
            VecOrOne::One(r#"["a.com","b.com"]"#.to_string()).as_list(),
            ["a.com", "b.com"]
        );
        assert_eq!(
            VecOrOne::One("a.com, b.com".to_string()).as_list(),
            ["a.com", "b.com"]
        );
        assert_eq!(
            VecOrOne::One("example.com".to_string()).as_list(),
            ["example.com"]
        );
    }

    #[test]
    fn many_keeps_entries_but_drops_empty_ones() {
        assert_eq!(
            VecOrOne::Many(vec!["a.com".to_string(), "".to_string()]).as_list(),
            ["a.com"]
        );
        assert_eq!(
            VecOrOne::Many(vec![" a.com ".to_string()]).as_list(),
            [" a.com "]
        );
    }

    #[test]
    fn is_nonempty_agrees_with_as_list() {
        for s in ["", "   ", "[]", r#"[""]"#, ","] {
            let v = VecOrOne::One(s.to_string());
            assert!(v.as_list().is_empty(), "{s:?} yields no entries");
            assert!(!v.is_nonempty(), "{s:?} must count as unset");
        }
        assert!(VecOrOne::One("a.com".to_string()).is_nonempty());
        assert!(VecOrOne::One(r#"["a.com"]"#.to_string()).is_nonempty());
        assert!(!VecOrOne::Many(vec![]).is_nonempty());
        assert!(!VecOrOne::Many(vec!["".to_string()]).is_nonempty());
        assert!(VecOrOne::Many(vec!["a.com".to_string()]).is_nonempty());
    }

    /// The consumer-visible form of every knob `canonicalize` touches — the
    /// same projection the B1 cache key makes (`as_list()` on the list fields,
    /// so a one/many difference is not a different request). Core cannot call
    /// `product::cache::canonical_query` (layering), so this is the local
    /// stand-in for "two requests became one".
    fn canonical_knobs(q: &SearchQuery) -> String {
        let list = |v: &Option<VecOrOne>| {
            v.as_ref()
                .map(|v| v.as_list().join(","))
                .unwrap_or_default()
        };
        format!(
            "query={:?}|mode={:?}|intent={:?}|strategy={:?}|provider={:?}|sources={}\
             |include={}|exclude={}|allowed={}|excluded={}|depth={:?}|range={:?}\
             |from={:?}|to={:?}",
            q.query,
            q.mode,
            q.intent,
            q.strategy,
            q.provider,
            q.sources
                .as_ref()
                .map(|s| s.as_list().join(","))
                .unwrap_or_default(),
            list(&q.include_domains),
            list(&q.exclude_domains),
            list(&q.allowed_x_handles),
            list(&q.excluded_x_handles),
            q.search_depth,
            q.time_range,
            q.from_date,
            q.to_date,
        )
    }

    #[test]
    fn canonicalize_folds_every_knob_spelling() {
        let mut q = SearchQuery {
            query: "  rust   async ".into(),
            mode: Some(" Web ".into()),
            intent: Some("COMPARISON".into()),
            strategy: Some(" Balanced_".into()),
            provider: Some(" Tavily ".into()),
            sources: Some(Sources::One(" Web , X ".into())),
            include_domains: Some(VecOrOne::One(r#"["*.AI.Meta.COM"]"#.into())),
            exclude_domains: Some(VecOrOne::Many(vec!["A.com , B.com".into()])),
            allowed_x_handles: Some(VecOrOne::One("@Foo".into())),
            search_depth: Some("ultra_fast".into()),
            time_range: Some("W".into()),
            ..Default::default()
        };
        q.canonicalize();
        assert_eq!(q.query, "rust async", "trimmed + collapsed, case kept");
        assert_eq!(q.mode.as_deref(), Some("web"));
        assert_eq!(q.intent.as_deref(), Some("comparison"));
        assert_eq!(q.strategy.as_deref(), Some("balanced"));
        assert_eq!(q.provider.as_deref(), Some("tavily"));
        assert_eq!(q.search_depth.as_deref(), Some("ultra-fast"));
        assert_eq!(q.time_range.as_deref(), Some("week"));
        assert_eq!(q.sources.as_ref().unwrap().as_list(), ["web", "x"]);
        assert_eq!(
            q.include_domains.as_ref().unwrap().as_list(),
            ["ai.meta.com"]
        );
        assert_eq!(
            q.exclude_domains.as_ref().unwrap().as_list(),
            ["a.com", "b.com"],
            "one blob entry becomes the two filters the client meant"
        );
        assert_eq!(q.allowed_x_handles.as_ref().unwrap().as_list(), ["foo"]);
    }

    /// THE formatter/validator split, on purpose: `normalize_choice` refuses a
    /// non-member so the boundary can 400 it, while `canonicalize` leaves it
    /// verbatim so that 400 names the bytes the client sent. Canonicalization
    /// alone must therefore never be used as a validator.
    #[test]
    fn formatter_leaves_banana_verbatim() {
        let mut q = SearchQuery {
            query: "ai".into(),
            mode: Some("banana".into()),
            provider: Some("Tavi".into()),
            search_depth: Some("turbo".into()),
            time_range: Some("fortnight".into()),
            from_date: Some("last tuesday".into()),
            ..Default::default()
        };
        q.canonicalize();
        assert_eq!(q.mode.as_deref(), Some("banana"));
        assert_eq!(q.provider.as_deref(), Some("Tavi"));
        assert_eq!(q.search_depth.as_deref(), Some("turbo"));
        assert_eq!(q.time_range.as_deref(), Some("fortnight"));
        assert_eq!(q.from_date.as_deref(), Some("last tuesday"));
    }

    /// A blank knob means "no opinion", not "pin it". This is a deliberate
    /// behavior choice, not a formatting side-effect: `resolve_strategy`'s
    /// `_ => Fast` arm reads `Some("  ")` as an EXPLICIT pin, so leaving the
    /// blank in place made one request mean three different things depending on
    /// surface (REST pinned Fast, MCP 400'd, and the cache keyed its own row).
    /// Clearing it makes every surface agree on auto-detect. The same blank on
    /// `provider` must never reach the dial as a pin either.
    #[test]
    fn blank_knobs_canonicalize_to_unset_not_to_a_pin() {
        for field in ["provider", "strategy", "mode", "intent", "search_depth"] {
            let mut q: SearchQuery = serde_json::from_value(serde_json::json!({
                "query": "ai",
                field: "   ",
            }))
            .unwrap_or_else(|e| panic!("{field}: {e}"));
            q.canonicalize();
            let value = match field {
                "provider" => &q.provider,
                "strategy" => &q.strategy,
                "mode" => &q.mode,
                "intent" => &q.intent,
                _ => &q.search_depth,
            };
            assert_eq!(value.as_deref(), None, "{field:?} blank must be unset");
        }
    }

    #[test]
    fn two_spellings_of_one_request_canonicalize_identically() {
        let strict = SearchQuery {
            query: "rust async".into(),
            mode: Some("web".into()),
            provider: Some("tavily".into()),
            sources: Some(Sources::Many(vec!["web".into(), "x".into()])),
            include_domains: Some(VecOrOne::Many(vec!["ai.meta.com".into()])),
            allowed_x_handles: Some(VecOrOne::Many(vec!["elonmusk".into()])),
            search_depth: Some("ultra-fast".into()),
            time_range: Some("week".into()),
            from_date: Some("2026-09-01".into()),
            ..Default::default()
        };
        let messy = SearchQuery {
            query: "rust    async".into(),
            mode: Some(" Web ".into()),
            provider: Some("Tavily".into()),
            sources: Some(Sources::One(" WEB , X".into())),
            include_domains: Some(VecOrOne::One(r#"["*.AI.Meta.com"]"#.into())),
            allowed_x_handles: Some(VecOrOne::One("@ElonMusk".into())),
            search_depth: Some("ultrafast".into()),
            time_range: Some("W".into()),
            from_date: Some("2026/9/1".into()),
            ..Default::default()
        };
        let (mut a, mut b) = (strict, messy);
        a.canonicalize();
        b.canonicalize();
        assert_eq!(canonical_knobs(&a), canonical_knobs(&b));
    }

    #[test]
    fn domain_entries_lose_wildcard_case_and_url_debris_but_keep_www() {
        let domains = |entries: VecOrOne| {
            let mut q = SearchQuery {
                query: "x".into(),
                include_domains: Some(entries),
                ..Default::default()
            };
            q.canonicalize();
            q.include_domains.unwrap().as_list()
        };
        assert_eq!(
            domains(VecOrOne::Many(vec!["*.ai.meta.com".into()])),
            ["ai.meta.com"]
        );
        assert_eq!(
            domains(VecOrOne::Many(vec!["A.com , B.com".into()])),
            ["a.com", "b.com"]
        );
        // A space is a boundary only when EVERY piece is a plausible host, so
        // two filters typed into one string still work.
        assert_eq!(
            domains(VecOrOne::Many(vec!["a.com b.com".into()])),
            ["a.com", "b.com"],
            "space-separated filters split when both pieces are hosts"
        );
        assert_eq!(
            domains(VecOrOne::Many(vec!["a.com,b.com c.com".into()])),
            ["a.com", "b.com", "c.com"],
            "comma and space in one blob"
        );
        // …and a mistyped single domain stays WHOLE, so the refusal the product
        // layer raises names what the client sent instead of the fragment "my".
        assert_eq!(
            domains(VecOrOne::Many(vec!["my site.com".into()])),
            ["my site.com"]
        );
        assert_eq!(
            domains(VecOrOne::Many(vec!["https://x.com/p".into()])),
            ["x.com"]
        );
        // `www.` is a vendor-visible difference, so it stays.
        assert_eq!(
            domains(VecOrOne::Many(vec![" WWW.Example.COM ".into()])),
            ["www.example.com"]
        );
        // Not a host: kept (trimmed) so the existing local refusal still names
        // it — canonicalize never drops or repairs an entry it cannot parse.
        assert_eq!(
            domains(VecOrOne::Many(vec!["not a host".into()])),
            ["not a host"]
        );
        // Only the leading wildcard is a no-op rewrite; an inner one is junk.
        assert_eq!(
            domains(VecOrOne::Many(vec!["foo.*.com".into()])),
            ["foo.*.com"]
        );
    }

    #[test]
    fn handles_lose_one_marker_and_lowercase() {
        let handles = |entries: VecOrOne| {
            let mut q = SearchQuery {
                query: "x".into(),
                allowed_x_handles: Some(entries),
                ..Default::default()
            };
            q.canonicalize();
            q.allowed_x_handles.unwrap().as_list()
        };
        assert_eq!(handles(VecOrOne::One("@Foo".into())), ["foo"]);
        assert_eq!(
            handles(VecOrOne::Many(vec!["@Foo , @BAR".into()])),
            ["foo", "bar"]
        );
        // EXACTLY one marker is stripped; a doubled one is not ours to
        // interpret, and a lone `@` cannot become an empty filter entry.
        assert_eq!(handles(VecOrOne::Many(vec!["@@foo".into()])), ["@foo"]);
        assert_eq!(handles(VecOrOne::Many(vec!["@".into()])), ["@"]);
    }

    #[test]
    fn query_collapses_whitespace_but_never_case_or_punctuation() {
        let mut q = SearchQuery {
            query: "  What's   C++ — \tNASA?\n ".into(),
            ..Default::default()
        };
        q.canonicalize();
        // One space where the run was; the em dash is punctuation, not
        // whitespace, and the mixed case is exactly what the client asked for.
        assert_eq!(q.query, "What's C++ — NASA?");
    }

    #[test]
    fn dates_pad_only_the_two_unambiguous_shapes() {
        let dates = |from: &str, to: &str| {
            let mut q = SearchQuery {
                query: "x".into(),
                from_date: Some(from.into()),
                to_date: Some(to.into()),
                ..Default::default()
            };
            q.canonicalize();
            (q.from_date, q.to_date)
        };
        for (input, want) in [
            ("2026-9-1", "2026-09-01"),
            ("2026/9/1", "2026-09-01"),
            (" 2026-12-3 ", "2026-12-03"),
            ("2026-09-01", "2026-09-01"),
        ] {
            let (from, _) = dates(input, input);
            assert_eq!(from.as_deref(), Some(want), "{input:?}");
        }
        // Anything unrecognized passes through byte for byte.
        for untouched in [
            "2026-13-45",
            "2026-02-30",
            "2026-09-01T00:00:00Z",
            "1757376000",
            "26-9-1",
            "2026/9-1",
            "last tuesday",
            "",
        ] {
            let (from, to) = dates(untouched, untouched);
            assert_eq!(from.as_deref(), Some(untouched), "from {untouched:?}");
            assert_eq!(to.as_deref(), Some(untouched), "to {untouched:?}");
        }
    }

    /// A blank knob is ABSENT — that is `normalize_choice`'s documented rule, so
    /// `canonicalize` has to reach the same verdict instead of leaving
    /// `Some("  ")` behind for routing to read as an explicit pin
    /// (`resolve_strategy`'s `_ => Fast`) and the cache to key as its own row.
    #[test]
    fn blank_knobs_canonicalize_to_unset() {
        let mut blank = SearchQuery {
            query: "x".into(),
            mode: Some("   ".into()),
            intent: Some("".into()),
            strategy: Some(" \t ".into()),
            provider: Some("  ".into()),
            search_depth: Some("  ".into()),
            time_range: Some("   ".into()),
            ..Default::default()
        };
        let unset = SearchQuery {
            query: "x".into(),
            ..Default::default()
        };
        blank.canonicalize();
        assert_eq!(canonical_knobs(&blank), canonical_knobs(&unset));
        assert_eq!(blank.strategy, None, "cleared, not left as Some(\"  \")");
        assert_eq!(blank.time_range, None);
    }

    /// The refusal side of the same rule: `"."` FOLDS to the empty string but
    /// was not blank on the way in, so it is a value the boundary refuses — and
    /// `canonicalize` must keep it verbatim rather than quietly unsetting it.
    #[test]
    fn a_value_that_only_folds_to_empty_is_kept() {
        let mut q = SearchQuery {
            query: "x".into(),
            mode: Some(".".into()),
            time_range: Some("..".into()),
            ..Default::default()
        };
        q.canonicalize();
        assert_eq!(q.mode.as_deref(), Some("."));
        assert_eq!(q.time_range.as_deref(), Some(".."));
    }

    /// A field the client left alone must come back exactly as it went in:
    /// canonicalization adds no defaults and clears no blanks.
    #[test]
    fn canonicalize_is_a_no_op_on_an_unset_query() {
        let mut q = SearchQuery {
            query: "plain".into(),
            ..Default::default()
        };
        q.canonicalize();
        assert_eq!(q.query, "plain");
        assert_eq!(q.mode, None);
        assert_eq!(q.provider, None);
        assert_eq!(q.sources.map(|s| s.as_list()), None);
        assert_eq!(q.include_domains.map(|d| d.as_list()), None);
        assert_eq!(q.allowed_x_handles.map(|h| h.as_list()), None);
        assert_eq!(q.search_depth, None);
        assert_eq!(q.time_range, None);
        assert_eq!(q.from_date, None);
    }
}
