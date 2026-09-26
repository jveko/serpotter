//! Closed-set validation **and canonicalization** for routing knobs, shared by
//! the REST and MCP entry points.
//!
//! Routing (`resolve.rs` / `rules.rs`) silently coerces unknown values
//! (strategy -> fast, mode -> no-op, intent -> pass-through), so both public
//! surfaces reject non-empty values outside the advertised sets instead of
//! letting them mislead the client. Moved out of `mcp/params.rs` so the REST
//! handlers validate identically (FU10).
//!
//! The matchers here are spelling-tolerant on purpose: `provider: " Tavily "`,
//! `"ultra_fast"` and `"ULTRAFAST"` all name exactly one advertised member, so
//! they are accepted and answered with the member's canonical spelling. One
//! fold (`canonical_choice`) plus one membership rule (`match_member`) is shared
//! by every knob except `time_range`, whose `d`/`w`/`m`/`y` are abbreviations
//! rather than spelling variants (see [`normalize_time_range`]), and
//! `SearchQuery::canonicalize` applies the same verdicts, so a value can never
//! be rewritten one way by the validator and another way by the cache key.

/// Advertised search modes (routing aliases `social` → xAI; `docs`/`github`/`pdf` → resource).
pub const VALID_MODES: &[&str] = &[
    "auto", "web", "news", "social", "docs", "research", "github", "pdf",
];
/// Advertised query intents.
pub const VALID_INTENTS: &[&str] = &[
    "auto",
    "factual",
    "status",
    "comparison",
    "tutorial",
    "exploratory",
    "news",
    "resource",
];
/// Advertised routing strategies.
pub const VALID_STRATEGIES: &[&str] = &["auto", "fast", "balanced", "verify", "deep"];
/// Advertised providers, plus `social` (routing aliases it to xai) and
/// `hybrid` (multi-provider web+x merge, REST-supported dial).
pub const VALID_PROVIDERS: &[&str] = &[
    "auto",
    "tavily",
    "firecrawl",
    "exa",
    "xai",
    "social",
    "hybrid",
];
/// Advertised search sources. `web`/`x` keep their routing semantics
/// (web web-leg / social x-leg); `social` is an alias for `x`; `news`
/// routes to Tavily news topic; `images` routes to Firecrawl image search
/// (categories). Unknown sources are client errors, never silent no-ops.
pub const VALID_SOURCES: &[&str] = &["web", "x", "social", "news", "images"];
/// Advertised Tavily search depths.
pub const VALID_SEARCH_DEPTHS: &[&str] = &["basic", "advanced", "fast", "ultra-fast"];
/// Deep modes are Exa server-side embeddings modes (B20/B29) — the deep
/// search leg triggers on `provider=exa` + one of these (or strategy=deep /
/// outputSchema). Non-exa providers never receive them (the product layer
/// maps them to `None` for web legs).
pub const VALID_DEEP_MODES: &[&str] = &["deep-lite", "deep", "deep-reasoning"];
/// Canonical `time_range` values. The long forms are what we forward; the
/// single letters Tavily documents for the same window (`d`/`w`/`m`/`y`) are
/// accepted as input spellings only — see [`normalize_time_range`].
pub const VALID_TIME_RANGES: &[&str] = &["day", "week", "month", "year"];
/// Extract-only provider set: firecrawl/tavily/exa support extract (B10 adds
/// Exa `/contents`); `auto` lets the chain detect (firecrawl first).
pub const VALID_EXTRACT_PROVIDERS: &[&str] = &["auto", "tavily", "firecrawl", "exa"];

/// Advertised extract `format` values (B27): `question` (firecrawl, single
/// URL) and `highlights` (exa, single URL) select those modes; `markdown`/
/// `text` are Tavily `/extract` output formats. Absent = plain scrape chain.
///
/// This is the ONE list both boundaries and the product route gate read
/// (through [`normalize_choice`]): the set used to be a literal pair — the
/// MCP boundary's `match` and the product dispatch's `match` arm, each
/// free to drift, and only one of them case-tolerant.
pub const VALID_EXTRACT_FORMATS: &[&str] = &["question", "highlights", "markdown", "text"];

/// Canonical spelling of a closed-set knob: trim, ASCII-lowercase, collapse
/// every run of `-`, `_` or whitespace into a single `-`, and drop trailing
/// dots. `'Tavily '`, `ultra_fast`, `ULTRA-FAST` and `ultra fast` all answer
/// `tavily` / `ultra-fast`.
///
/// The fold is deliberately lossy in one direction only: it can never turn a
/// value into a *different* word, only re-space and re-case it, so applying it
/// to something outside every closed set is harmless (the caller refuses the
/// fold, and `SearchQuery::canonicalize` leaves the value verbatim).
pub fn canonical_choice(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_sep = false;
    for c in value.trim().chars() {
        if c == '-' || c == '_' || c.is_whitespace() {
            // One separator, and never a leading one: `-fast` means `fast`.
            pending_sep = !out.is_empty();
            continue;
        }
        if pending_sep {
            out.push('-');
        }
        pending_sep = false;
        out.push(c.to_ascii_lowercase());
    }
    // `example.com.` is the same host as `example.com`; a trailing dot is the
    // DNS root hint, not part of the value any of our sets carries.
    out.trim_end_matches('.').to_string()
}

/// How one client spelling relates to a closed set. Private to this module:
/// the public surface of the rule is [`normalize_choice`] /
/// [`normalize_sources`], and `SearchQuery::canonicalize` reaches it the same
/// way the boundary does — through `normalize_choice`.
#[derive(Debug, PartialEq)]
enum Folded<'a> {
    /// Exactly one member matches; the payload is that member's canonical
    /// spelling (never the caller's).
    Member(&'a str),
    /// Two or more members share a separator-free key, so the value cannot be
    /// resolved without guessing.
    Ambiguous(Vec<&'a str>),
    /// No member matches under any accepted spelling.
    Unknown,
}

/// The one membership rule every matcher uses.
///
/// Two steps, in this order:
/// 1. the canonical fold of [`canonical_choice`] compared exactly;
/// 2. only if that misses, the same fold compared with every separator removed
///    (`ultrafast` → `ultra-fast`, `high context` → `high-context`).
///
/// Step 2 is a rule, not an alias table, so it covers spellings nobody has
/// curated yet — and it is guarded by uniqueness: it accepts only when exactly
/// one member survives, because a set that ever grew two members differing
/// solely by separators (say `fast-read` and `fastread`) must refuse rather
/// than silently pick one. Nothing in today's sets is within a separator of
/// another member, which is what makes step 2 safe to run on all of them.
fn match_member<'a>(value: &str, valid: &'a [&'a str]) -> Folded<'a> {
    let canonical = canonical_choice(value);
    if let Some(member) = valid
        .iter()
        .copied()
        .find(|member| *member == canonical.as_str())
    {
        return Folded::Member(member);
    }
    // An all-separator input (`.`, `--`) has no key to compare: it is junk, and
    // must not fold onto the empty key of a hypothetical empty member.
    if canonical.is_empty() {
        return Folded::Unknown;
    }
    let key = separator_free_key(&canonical);
    let hits: Vec<&'a str> = valid
        .iter()
        .copied()
        .filter(|member| separator_free_key(member) == key)
        .collect();
    match hits.len() {
        0 => Folded::Unknown,
        1 => Folded::Member(hits[0]),
        _ => Folded::Ambiguous(hits),
    }
}

/// Comparison key for step 2 of [`match_member`]: the canonical value with every
/// separator removed. Lowercasing and trimming already happened.
fn separator_free_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| *c != '-' && *c != '_' && !c.is_whitespace())
        .collect()
}

/// Lenient replacement for the old exact-match `validate_choice`: accepts any
/// spelling of a valid member and returns that member's canonical form.
/// `None`/empty/whitespace-only means "unset" → `Ok(None)`, which is how both
/// surfaces already route an absent knob.
///
/// The error text keeps the historical shape (`{field}: {v:?} is not a
/// supported value (valid: …)`) and quotes the RAW value, so a client that
/// sends junk it did mean to fix still sees what it sent. Note the deliberate
/// difference from `SearchQuery::canonicalize`: this refuses `"banana"`, the
/// formatter leaves it verbatim — a validator cannot rewrite what it rejects,
/// and the boundary error has to name the client's own bytes.
pub fn normalize_choice(
    field: &str,
    value: Option<&str>,
    valid: &[&str],
) -> Result<Option<String>, String> {
    let Some(raw) = value else {
        return Ok(None);
    };
    // Unset is decided on the raw input: a blank knob was never meant to be a
    // value, and folding `"."` to the empty string must not invent one either.
    if raw.trim().is_empty() {
        return Ok(None);
    }
    match match_member(raw, valid) {
        Folded::Member(member) => Ok(Some(member.to_string())),
        Folded::Ambiguous(candidates) => Err(format!(
            "{field}: {raw:?} is ambiguous between {} — spell one of them exactly",
            candidates.join(", ")
        )),
        Folded::Unknown => Err(format!(
            "{field}: {raw:?} is not a supported value (valid: {})",
            valid.join(", ")
        )),
    }
}

/// True when `value` is one of the Exa deep-search modes (B20/B29). The deep
/// modes are distinct from the Tavily depths in [`VALID_SEARCH_DEPTHS`]; they
/// select the Exa server-side embeddings leg.
///
/// EXACT match by design: every caller reads this after
/// [`crate::SearchQuery::canonicalize`] has folded the knob, so re-folding here
/// would hide a caller that forgot to canonicalize instead of failing loudly.
pub fn is_deep_mode(value: Option<&str>) -> bool {
    matches!(
        value,
        Some("deep-lite") | Some("deep") | Some("deep-reasoning")
    )
}

/// Lenient `validate_search_depth` replacement: either a Tavily depth
/// (`basic`/`advanced`/`fast`/`ultra-fast`) or an Exa deep mode (`deep-lite`/
/// `deep`/`deep-reasoning`), in any unambiguous spelling, returned canonical —
/// so `deep_reasoning` and `DEEP-REASONING` both reach the deep leg as
/// `deep-reasoning`.
///
/// One matcher over the union of the two sets: the old code already listed both
/// in its error, and accepting a depth in one set that means something else in
/// the other is exactly the ambiguity [`match_member`] refuses.
pub fn normalize_search_depth(field: &str, value: Option<&str>) -> Result<Option<String>, String> {
    let depths: Vec<&str> = VALID_SEARCH_DEPTHS
        .iter()
        .chain(VALID_DEEP_MODES.iter())
        .copied()
        .collect();
    normalize_choice(field, value, &depths)
}

/// Lenient `validate_sources` replacement: canonicalize every entry of a
/// `sources` list against [`VALID_SOURCES`]. Empty entries are tolerated and
/// fold away (routing treats them as absent, and dropping them is what makes
/// `["web", ""]` and `["web"]` one request). `social` canonicalizes to `x`
/// because routing compares source legs by those canonical names; duplicates
/// then collapse without changing the first occurrence's order.
pub fn normalize_sources(field: &str, values: &[String]) -> Result<Vec<String>, String> {
    let mut out = Vec::with_capacity(values.len());
    for raw in values {
        if raw.trim().is_empty() {
            continue;
        }
        let member = match match_member(raw, VALID_SOURCES) {
            Folded::Member(member) => member,
            Folded::Ambiguous(candidates) => {
                return Err(format!(
                    "{field}: {raw:?} is ambiguous between {} — spell one of them exactly",
                    candidates.join(", ")
                ))
            }
            Folded::Unknown => {
                return Err(format!(
                    "{field}: {raw:?} is not a supported source (valid: {})",
                    VALID_SOURCES.join(", ")
                ))
            }
        };
        let canonical = if member == "social" { "x" } else { member };
        if !out.iter().any(|existing| existing == canonical) {
            out.push(canonical.to_string());
        }
    }
    Ok(out)
}

/// Normalize the `time_range` knob — historically forwarded to the vendors
/// completely unvalidated, which made `"W"`, `"week"` and `"WEEK "` three
/// different upstream bodies and three different cache rows for one request.
///
/// The long forms are canonical; Tavily's documented single-letter short forms
/// (`d`/`w`/`m`/`y`) are accepted input spellings. Anything else is a `400`:
/// that is the one deliberate NEW refusal in this surface, and it replaces a
/// silently-forwarded junk value the vendor would either ignore or reject on
/// our quota, so an agent never learns which half of its request was wrong.
///
/// Deliberately NOT built on [`normalize_choice`]: `d`/`w`/`m`/`y` are
/// abbreviations, not case-or-separator variants, so the separator-free fold
/// can never derive `day` from `d` — delegating would silently drop the short
/// forms Tavily documents. Two small matchers with a shared fold beat one
/// abstraction that has to model both kinds of renaming.
pub fn normalize_time_range(field: &str, value: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = value else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let member = match canonical_choice(raw).as_str() {
        "day" | "d" => Some("day"),
        "week" | "w" => Some("week"),
        "month" | "m" => Some("month"),
        "year" | "y" => Some("year"),
        _ => None,
    };
    match member {
        Some(member) => Ok(Some(member.to_string())),
        None => Err(format!(
            "{field}: {raw:?} is not a supported value (valid: {})",
            VALID_TIME_RANGES.join(", ")
        )),
    }
}

/// Refuse the research knobs the DEEP loop drops (shared by REST and MCP so
/// the two surfaces cannot disagree about one request).
///
/// Takes `deep` itself and is a no-op when it is false, so a caller cannot
/// apply this rule to a standard-path request by omission: the two entry
/// points pass their own flag and the deep/combination condition is not
/// re-derived (and cannot be re-derived wrongly) at each site.
///
/// `deep` is a different product loop (search → scrape → xAI synthesis), and
/// `research_inner` branches on it FIRST: a `deep` request naming the Tavily
/// backend is answered by the serpotter loop with the backend silently
/// discarded, the deep loop never sends a `citation_format` to Tavily at all,
/// and it has no xAI leg to spend `social_max_results` on (it only records a
/// warning string). The schema advertised all three, so the request was
/// accepted and quietly answered by a different product. Refusing the
/// combination names what was dropped instead.
///
/// Blanks are UNSET, not refusals: `normalize_choice` answers `Ok(None)` for
/// `""`/whitespace and `research_inner` filters them to `None`, so a client
/// that sent a defaulted knob must not be told it conflicts. `social_max_results: 0`
/// is likewise legal — it is the documented "social disabled" no-op, not a
/// dropped request — and `scrape_top_n` is absent from this rule on purpose:
/// the deep loop HONORS an explicit 0 (its clamp keeps 0 as "scrape nothing")
/// and clamps anything above its own budget with a note in the response, so
/// that knob works rather than vanishing. That rule is about the STRING knobs
/// only; it has no bearing on the bool below.
///
/// `include_content` is refused for EITHER value — presence is the only
/// honest test here, so an SDK that always serializes the optional bool MUST
/// OMIT the field on a deep request rather than send `false`. Unlike a string
/// knob there is no `normalize_choice` folding step to turn a blank into
/// "unset": `Some(false)` survives all the way down (`research_inner` reads
/// `unwrap_or(false)`), so it is indistinguishable from a real ask. And the
/// deep loop cannot honor either value: its search legs hard-code
/// `include_content: Some(false)` (the synthesis needs URLs, not vendor page
/// text — the scrapes carry the content) while its scrapes hard-code full
/// content (`scraped_page_from_extract(..., true)`, the only grounded input the
/// xAI synthesis reads). So `false` was silently ignored (the caller paid for
/// full page text it did not want) and `true` was silently ignored too
/// (search hits came back contentless) — the knob was INVERTED relative to
/// the ask, on a path that advertised the field. Refusing names it instead.
pub fn validate_deep_research_knobs(
    deep: bool,
    research_backend: Option<&str>,
    citation_format: Option<&str>,
    social_max_results: Option<u32>,
    include_content: Option<bool>,
) -> Result<(), String> {
    if !deep {
        return Ok(());
    }
    for (present, named) in [
        (
            research_backend.is_some_and(|v| !v.trim().is_empty()),
            "researchBackend",
        ),
        (
            citation_format.is_some_and(|v| !v.trim().is_empty()),
            "citationFormat",
        ),
    ] {
        if present {
            return Err(format!(
                "deep: {named} is ignored by the deep research loop; \
                 send deep without {named} (or drop deep)"
            ));
        }
    }
    if social_max_results.is_some_and(|n| n > 0) {
        return Err(
            "deep: socialMaxResults is ignored by the deep research loop (no xAI leg); \
             send deep without socialMaxResults (or 0)"
                .into(),
        );
    }
    // `is_some`, not the boolean: the deep path honors NEITHER value (its
    // search legs are always contentless, its scrapes always full), so even an
    // explicit `false` is a knob that cannot be delivered.
    if include_content.is_some() {
        return Err(
            "deep: includeContent cannot be honored by the deep research loop \
             (search hits are always contentless; scraped pages are always full); \
             send deep without includeContent (or drop deep)"
                .into(),
        );
    }
    Ok(())
}

/// Advertised research backends. `tavily` selects Tavily's single `/research`
/// job; unset/`serpotter` is the serpotter loop. Shared by both boundaries so
/// a backend cannot be advertised on one surface and refused on the other.
pub const VALID_RESEARCH_BACKENDS: &[&str] = &["serpotter", "tavily"];
/// Advertised Tavily research citation formats. Cosmetic for the serpotter
/// backend (its citations already exist — it does not reformat them), which
/// is why `citation_format` alongside a non-Tavily backend is accepted here
/// and only refused for `deep` (see [`validate_deep_research_knobs`]).
pub const VALID_CITATION_FORMATS: &[&str] = &["numbered", "mla", "apa", "chicago"];

/// Split a client-supplied bare-string list blob on commas.
///
/// Deliberately NOT whitespace: a hostname can't contain a comma, so a comma is
/// always a boundary, but a space is ambiguous between "two filters typed into
/// one string" and "one mistyped filter". Splitting unconditionally would make
/// `"my site.com"` report a refusal of `"my"` instead of naming what the client
/// actually sent. The domain fold does that split conditionally, where it can
/// check plausibility — see `canonical_domain_entries` in `types.rs`.
fn split_separators(s: &str) -> impl Iterator<Item = &str> {
    s.split(',').map(str::trim).filter(|s| !s.is_empty())
}

/// Coerce a client-supplied list value into discrete trimmed entries.
/// A JSON array literal is parsed and its elements kept ATOMIC
/// (`["a","b"]` → a, b); a bare string splits on **commas only**
/// (`"a, b"` → a, b); a plain single value passes through. Empty entries
/// dropped. Never panics; invalid JSON falls back to comma-splitting.
///
/// Whitespace is deliberately not split here — a space cannot be judged without
/// knowing what the field means, so `types.rs::fold_domain_entry` does it
/// conditionally for domain filters (one entry per piece only when every piece
/// is a plausible host), which is what lets a mistyped single domain stay whole
/// and get named verbatim in the refusal.
///
/// Agents routinely stringify arrays they meant to send as JSON: the whole
/// `["a.com", "b.com"]` arrives as ONE string. Without this, `VecOrOne::One`
/// hands that blob to the vendor as a single domain and Tavily/Exa answer with
/// a 400 that we used to report as a retryable provider failure.
pub fn split_list_field(input: &str) -> Vec<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    // Bracket-guard the parse: a bare `123` or `"text"` is valid JSON but is
    // not a list, and must fall through to the separator path unchanged.
    let parsed = if trimmed.starts_with('[') {
        serde_json::from_str::<serde_json::Value>(trimmed).ok()
    } else {
        None
    };
    if let Some(serde_json::Value::Array(items)) = parsed {
        // Elements are trimmed but NOT re-split: inside an array the client has
        // already marked the boundaries, so a compound value keeps its commas
        // and spaces. Non-string elements become their JSON text rather than
        // being dropped: `[1,2]` must surface as implausible entries the
        // provider refuses loudly, not as a silently empty filter.
        return items
            .into_iter()
            .map(|item| match item {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            })
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    split_separators(trimmed).map(str::to_string).collect()
}

/// Normalize a vendor domain-filter entry to a bare lowercase hostname:
/// strips scheme, userinfo, path, query, fragment, port, surrounding
/// whitespace/quotes, and a trailing dot. Keeps `www.` (vendor semantics).
/// `None` when the result is not a plausible hostname: any char outside
/// [a-z0-9.-] (rejects `_`, spaces, `/`, `%`), fewer than two labels, an empty
/// label, or a final label with no ASCII letter (so dotted-quad IPv4 → `None`).
///
/// Tavily, Exa and Firecrawl all 400 a filter entry that still carries a
/// protocol or a path ("Domain must be a valid hostname without protocol or
/// path"), so callers either send this function's output or refuse locally.
pub fn normalize_domain_filter(input: &str) -> Option<String> {
    // Clients wrap entries in quotes when they hand-build JSON-ish strings.
    let s = input.trim().trim_matches(|c: char| c == '\'' || c == '"');
    let after_scheme = s.split_once("://").map_or(s, |(_, rest)| rest);
    // The authority ends at the first path/query/fragment delimiter.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // `user:pass@host` — the credentials can contain ':' so strip them first.
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    // hostname:port — the ':' is never part of a DNS name we forward.
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let host = host.trim().trim_end_matches('.').to_lowercase();
    // DNS-safe characters only; `_`, `%`, spaces and `/` are junk the vendors
    // would answer with a 400.
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    // Plausibility on top of the char class: two or more non-empty labels and a
    // final label carrying at least one letter. This is what refuses the
    // dotted-quad IPv4 form (`192.168.1.1`), `.com` and `a..com` residues —
    // shapes no vendor accepts in a domain filter. `rsplit_once` also encodes
    // the "must contain a dot" rule (`localhost` → `None`).
    let (prefix, tld) = host.rsplit_once('.')?;
    if prefix.is_empty()
        || tld.is_empty()
        || !tld.bytes().any(|b| b.is_ascii_alphabetic())
        || prefix.split('.').any(|label| label.is_empty())
    {
        return None;
    }
    Some(host)
}

/// Canonicalize a client's `country` knob into the exact token a vendor accepts
/// (`ID`/`id`/`Indonesia`/`indonesia` -> `indonesia`), or `None` when it names no
/// country the vendor can filter on.
///
/// The table lives in `country.rs` and is keyed on Tavily's documented CLOSED
/// `country` enum, not on ISO short names: the accepted value is `czech
/// republic`, where ISO says `Czechia`, and `south korea`, where ISO says
/// `Korea, Republic of`. A shape check alone would forward `Nonsenseland`-class
/// junk (a vendor 400 that burns a key) and ISO-keying would reject values the
/// vendor accepts, so membership in that table is the only rule. Codes resolve
/// through `country_name`, names/aliases through `canonical_country_name`
/// (case-, accent- and whitespace-insensitive); both return the vendor's token.
pub fn normalize_country_filter(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    crate::country::country_name(trimmed)
        .or_else(|| crate::country::canonical_country_name(trimmed))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one fold every matcher shares: surrounding space, case, separator
    /// choice and a trailing dot are noise. Idempotence matters as much as the
    /// fold — `SearchQuery::canonicalize` runs on every request and must never
    /// rewrite a value twice into something else.
    #[test]
    fn canonical_choice_folds_spelling_not_meaning() {
        for (input, want) in [
            (" Tavily ", "tavily"),
            ("ultra_fast", "ultra-fast"),
            ("ULTRA-FAST", "ultra-fast"),
            ("ultra fast", "ultra-fast"),
            ("deep_reasoning", "deep-reasoning"),
            ("-fast", "fast"),
            ("example.com.", "example.com"),
            ("WEEK", "week"),
            ("", ""),
            ("   ", ""),
        ] {
            let folded = canonical_choice(input);
            assert_eq!(folded, want, "{input:?}");
            assert_eq!(
                canonical_choice(&folded),
                folded,
                "{input:?} fold is not idempotent"
            );
        }
    }

    /// The full spelling lattice for a hyphenated member: every cell lands on
    /// the SAME canonical member, including the separator-free `ultrafast` and
    /// `deepreasoning` — resolved by the uniqueness-guarded second step, not by
    /// a per-value alias table.
    #[test]
    fn every_spelling_of_one_member_resolves_identically() {
        for (spelling, want) in [
            ("ultra-fast", "ultra-fast"),
            ("ultra fast", "ultra-fast"),
            ("ultra_fast", "ultra-fast"),
            ("ULTRA-FAST", "ultra-fast"),
            (" ULTRAFAST ", "ultra-fast"),
            ("ultrafast", "ultra-fast"),
            ("deep-reasoning", "deep-reasoning"),
            ("deep reasoning", "deep-reasoning"),
            ("DEEP_REASONING", "deep-reasoning"),
            ("deepreasoning", "deep-reasoning"),
        ] {
            assert_eq!(
                normalize_search_depth("search_depth", Some(spelling))
                    .expect("accepted")
                    .expect("a value, not unset"),
                want,
                "{spelling:?}"
            );
        }
    }

    #[test]
    fn none_and_empty_are_unset() {
        for unset in [None, Some(""), Some("   ")] {
            assert_eq!(normalize_choice("mode", unset, VALID_MODES).unwrap(), None);
        }
    }

    #[test]
    fn valid_value_passes() {
        // The answer is the MEMBER's spelling, never the caller's.
        assert_eq!(
            normalize_choice("strategy", Some("balanced"), VALID_STRATEGIES)
                .unwrap()
                .as_deref(),
            Some("balanced")
        );
        assert_eq!(
            normalize_choice("provider", Some(" Tavily "), VALID_PROVIDERS)
                .unwrap()
                .as_deref(),
            Some("tavily")
        );
        assert_eq!(
            normalize_choice("provider", Some("hybrid"), VALID_PROVIDERS)
                .unwrap()
                .as_deref(),
            Some("hybrid")
        );
        assert_eq!(
            normalize_choice("provider", Some("EXA"), VALID_EXTRACT_PROVIDERS)
                .unwrap()
                .as_deref(),
            Some("exa")
        );
    }

    #[test]
    fn invalid_value_lists_field_and_valid_set() {
        let err = normalize_choice("strategy", Some("bogus"), VALID_STRATEGIES)
            .expect_err("bogus must fail");
        assert!(err.contains("strategy"), "{err}");
        assert!(err.contains("\"bogus\""), "raw value: {err}");
        assert!(err.contains("balanced"), "valid set listed: {err}");
        // A value that only FOLDS to nothing is still refused, not treated as
        // unset: `"."` was never a member and must not silently become one.
        assert!(normalize_choice("mode", Some("."), VALID_MODES).is_err());
    }

    // ---- B11: sources allowlist (news/images) ----

    #[test]
    fn valid_sources_accept_web_x_social_news_images() {
        for s in ["web", "x", "news", "images"] {
            assert!(VALID_SOURCES.contains(&s), "VALID_SOURCES must include {s}");
            assert_eq!(normalize_sources("sources", &[s.to_string()]).unwrap(), [s]);
        }
        assert!(VALID_SOURCES.contains(&"social"));
        assert_eq!(
            normalize_sources("sources", &["social".to_string()]).unwrap(),
            ["x"]
        );
    }

    /// Routing compares source legs with `== "x"` / `== "web"` (xai.rs), so the
    /// advertised `social` alias must reach hybrid detection as canonical `x`.
    /// Deduplication happens after that fold, so both spellings cannot leave two
    /// copies of the same leg behind.
    #[test]
    fn social_alias_folds_before_hybrid_detection_and_deduplicates() {
        let values = ["web", "social"].map(String::from);
        let normalized = normalize_sources("sources", &values).unwrap();
        assert_eq!(normalized, ["web", "x"]);
        assert!(normalized.contains(&"web".to_string()));
        assert!(normalized.contains(&"x".to_string()));
        assert!(!normalized.iter().any(|source| source == "social"));

        assert_eq!(
            normalize_sources(
                "sources",
                &["web".to_string(), "x".to_string(), "social".to_string()]
            )
            .unwrap(),
            ["web", "x"]
        );
    }

    #[test]
    fn sources_fold_to_canonical_entries() {
        assert_eq!(
            normalize_sources("sources", &[" Web ".to_string(), "X".to_string()]).unwrap(),
            ["web", "x"]
        );
    }

    #[test]
    fn normalize_sources_rejects_unknown_value() {
        let err = normalize_sources("sources", &["banana".to_string()])
            .expect_err("banana is not a source");
        assert!(err.contains("sources"), "{err}");
        assert!(err.contains("banana"), "{err}");
        assert!(err.contains("news"), "valid set listed: {err}");
    }

    #[test]
    fn normalize_sources_tolerates_empty_and_unset() {
        assert!(normalize_sources("sources", &[]).unwrap().is_empty());
        assert!(normalize_sources("sources", &["".to_string()])
            .unwrap()
            .is_empty());
        // Blanks fold away instead of echoing back an entry routing ignores.
        assert_eq!(
            normalize_sources("sources", &["web".into(), "".into(), "  ".into()]).unwrap(),
            ["web"]
        );
    }

    // ---- B10: Exa joins the extract provider set ----

    #[test]
    fn extract_providers_include_exa() {
        assert!(
            VALID_EXTRACT_PROVIDERS.contains(&"exa"),
            "exa must be a valid extract provider (B10)"
        );
        assert_eq!(
            normalize_choice("provider", Some("Exa"), VALID_EXTRACT_PROVIDERS)
                .unwrap()
                .as_deref(),
            Some("exa")
        );
    }

    #[test]
    fn sets_are_disjoint_and_complete() {
        // Providers must cover the search dispatch surface plus aliases.
        for p in [
            "auto",
            "tavily",
            "firecrawl",
            "exa",
            "xai",
            "social",
            "hybrid",
        ] {
            assert!(VALID_PROVIDERS.contains(&p), "missing provider {p}");
        }
        // Extract providers are a strict subset of search providers.
        for p in VALID_EXTRACT_PROVIDERS {
            assert!(
                VALID_PROVIDERS.contains(p),
                "extract provider {p} not in search set"
            );
        }
        // The separator-free second step of `match_member` is only safe while no
        // two advertised members of one set collide with each other once their
        // separators are dropped. This is the tripwire for the day someone adds
        // `fast-read` next to `fastread` and the fold starts guessing.
        for set in [
            VALID_MODES,
            VALID_INTENTS,
            VALID_STRATEGIES,
            VALID_PROVIDERS,
            VALID_SOURCES,
            VALID_EXTRACT_PROVIDERS,
            VALID_TIME_RANGES,
        ] {
            let keys: Vec<String> = set.iter().map(|m| separator_free_key(m)).collect();
            let unique: std::collections::BTreeSet<&str> =
                keys.iter().map(String::as_str).collect();
            assert_eq!(
                keys.len(),
                unique.len(),
                "members {set:?} collide once separators are dropped"
            );
        }
        // The depth union is matched as ONE set, so it must be collision-free too.
        let depths: Vec<&str> = VALID_SEARCH_DEPTHS
            .iter()
            .chain(VALID_DEEP_MODES.iter())
            .copied()
            .collect();
        let keys: Vec<String> = depths.iter().map(|m| separator_free_key(m)).collect();
        let unique: std::collections::BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        assert_eq!(
            keys.len(),
            unique.len(),
            "Tavily depths and Exa deep modes collide"
        );
    }

    // ---- stringified-list coercion (the tavily/exa 400 on include_domains) ----

    #[test]
    fn split_list_field_coerces_a_json_array_literal() {
        assert_eq!(
            split_list_field(r#"["ai.meta.com", "dev.meta.ai"]"#),
            ["ai.meta.com", "dev.meta.ai"]
        );
        // Elements may themselves contain commas: the JSON parse must win.
        assert_eq!(
            split_list_field(r#"["Bonaire, Sint Eustatius and Saba"]"#),
            ["Bonaire, Sint Eustatius and Saba"]
        );
        assert_eq!(split_list_field("[]"), Vec::<String>::new());
    }

    #[test]
    fn split_list_field_coerces_a_comma_separated_string() {
        assert_eq!(split_list_field("a.com, b.com"), ["a.com", "b.com"]);
        assert_eq!(split_list_field(" a.com ,, b.com ,"), ["a.com", "b.com"]);
        // The shape a client actually sends: comma PLUS spaces on both sides.
        // Case survives splitting and is the field canonicalizer's job.
        assert_eq!(split_list_field("A.com , B.com"), ["A.com", "B.com"]);
        // Spaces are NOT split here. A comma can never appear in a hostname, so
        // it is always a boundary; a space is ambiguous between two filters and
        // one mistyped filter, so the split is deferred to `fold_domain_entry`,
        // which can test each piece for plausibility before committing.
        assert_eq!(split_list_field("a.com b.com"), ["a.com b.com"]);
        assert_eq!(
            split_list_field("a.com,b.com c.com"),
            ["a.com", "b.com c.com"],
            "the comma splits; the space inside the second piece does not"
        );
    }

    #[test]
    fn split_list_field_keeps_a_plain_single_value() {
        assert_eq!(split_list_field("example.com"), ["example.com"]);
        // A scalary JSON-looking value is NOT a list: it stays untouched so the
        // caller can normalize/refuse it instead of silently losing it.
        assert_eq!(split_list_field("123"), ["123"]);
        assert_eq!(split_list_field("\"example.com\""), ["\"example.com\""]);
    }

    #[test]
    fn split_list_field_drops_nothing_silently_on_bad_json() {
        // Unclosed array: fall back to comma-splitting, brackets and all, so the
        // provider sees implausible entries and refuses loudly.
        assert_eq!(
            split_list_field(r#"["a.com", "b.com""#),
            ["[\"a.com\"", "\"b.com\""]
        );
        assert_eq!(split_list_field(r#"["1.2.3", 5]"#), ["1.2.3", "5"]);
    }

    #[test]
    fn split_list_field_empty_and_whitespace_only_are_no_entries() {
        assert!(split_list_field("").is_empty());
        assert!(split_list_field("   \n\t ").is_empty());
    }

    // ---- domain-filter normalization (firecrawl "hostname without protocol") ----

    #[test]
    fn domain_filter_strips_scheme_path_query_and_fragment() {
        assert_eq!(
            normalize_domain_filter("https://ai.meta.com/foo?x=1").as_deref(),
            Some("ai.meta.com")
        );
        assert_eq!(
            normalize_domain_filter("http://dev.meta.ai#top").as_deref(),
            Some("dev.meta.ai")
        );
        assert_eq!(
            normalize_domain_filter("example.com/a/b/").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn domain_filter_strips_userinfo_and_port() {
        assert_eq!(
            normalize_domain_filter("https://user:pw@ex.com:8443/x").as_deref(),
            Some("ex.com")
        );
        assert_eq!(
            normalize_domain_filter("example.com:443").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn domain_filter_lowercases_and_keeps_www() {
        assert_eq!(
            normalize_domain_filter("WWW.Example.COM").as_deref(),
            Some("www.example.com")
        );
    }

    #[test]
    fn domain_filter_strips_quotes_and_trailing_dot() {
        assert_eq!(
            normalize_domain_filter("\"example.com\"").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_domain_filter("'example.com.'").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn domain_filter_refuses_empty_junk_and_ip_literals() {
        for junk in [
            "",
            "   ",
            "localhost",   // no dot
            "example",     // no dot
            "foo_bar.com", // underscore
            "a b.com",     // space
            "bad/path",    // slash with nothing plausible before it
            "/not/a/domain",
            "https://",
            "192.168.1.1", // IPv4-ish: no alphabetic TLD
            "10.0.0",      // numeric last label
            ".com",        // empty first label
            "a..com",      // empty middle label
            "例え.jp",     // non-DNS characters
            "ex.com%00",
            "a.com,b.com", // must go through split_list_field first
        ] {
            assert_eq!(
                normalize_domain_filter(junk),
                None,
                "{junk:?} must be refused locally"
            );
        }
    }

    // ---- country-filter normalization (tavily 400 "Invalid country") ----

    #[test]
    fn country_filter_maps_iso2_codes_to_vendor_tokens() {
        assert_eq!(normalize_country_filter("ID").as_deref(), Some("indonesia"));
        assert_eq!(normalize_country_filter("id").as_deref(), Some("indonesia"));
        assert_eq!(
            normalize_country_filter(" us ").as_deref(),
            Some("united states")
        );
        assert_eq!(
            normalize_country_filter("GB").as_deref(),
            Some("united kingdom")
        );
    }

    /// Tavily's enum is its own set of lowercase common names: ISO short names
    /// (`Viet Nam`, `Korea, Republic of`) and friendly aliases (`South Korea`)
    /// must canonicalize to the token, never be refused and never be echoed
    /// back in ISO spelling.
    #[test]
    fn country_filter_canonicalizes_names_and_aliases() {
        for (input, token) in [
            ("indonesia", "indonesia"),
            ("United States", "united states"),
            ("Viet Nam", "vietnam"),
            ("vietnam", "vietnam"),
            ("South Korea", "south korea"),
            ("Korea, Republic of", "south korea"),
            ("Czechia", "czech republic"),
            ("Russian Federation", "russia"),
            ("Brunei Darussalam", "brunei"),
        ] {
            assert_eq!(
                normalize_country_filter(input).as_deref(),
                Some(token),
                "{input:?} must canonicalize to {token:?}"
            );
        }
    }

    #[test]
    fn country_filter_folds_accents_case_and_whitespace() {
        assert_eq!(
            normalize_country_filter("Türkiye").as_deref(),
            Some("turkey")
        );
        assert_eq!(
            normalize_country_filter("turkiye").as_deref(),
            Some("turkey")
        );
        assert_eq!(
            normalize_country_filter("SOUTH    KOREA").as_deref(),
            Some("south korea")
        );
        assert_eq!(
            normalize_country_filter("côte d’ivoire").as_deref(),
            None,
            "Côte d'Ivoire is not in the vendor enum at all"
        );
    }

    #[test]
    fn country_filter_refuses_unknown_and_malformed_values() {
        for junk in [
            "",
            "   ",
            "Nonsenseland",
            r#"["Indonesia"]"#,
            "id,id",
            "ZZ",
            "Palestine",
            "Ivory Coast and a name far too long for any country in the world",
        ] {
            assert_eq!(
                normalize_country_filter(junk),
                None,
                "{junk:?} must be refused locally, not burned on a vendor 400"
            );
        }
    }

    #[test]
    fn country_filter_refuses_a_60_char_name() {
        let long = format!("{}land", "a".repeat(56));
        assert_eq!(long.chars().count(), 60);
        assert_eq!(normalize_country_filter(&long), None);
    }

    /// `search_depth` is one knob with two vendor dialects — the Tavily depths
    /// and the Exa deep modes — and both stay accepted, matched as one set.
    #[test]
    fn search_depth_accepts_both_provider_dialects() {
        for (input, want) in [
            ("Advanced", "advanced"),
            (" basic ", "basic"),
            (" FAST ", "fast"),
            ("ultra_fast", "ultra-fast"),
            ("deep_reasoning", "deep-reasoning"),
            ("DEEP-LITE", "deep-lite"),
            ("deep", "deep"),
        ] {
            assert_eq!(
                normalize_search_depth("search_depth", Some(input))
                    .unwrap()
                    .as_deref(),
                Some(want),
                "{input:?}"
            );
        }
        assert_eq!(normalize_search_depth("search_depth", None).unwrap(), None);
        assert_eq!(
            normalize_search_depth("search_depth", Some("  ")).unwrap(),
            None,
            "blank is unset, not a depth"
        );
        assert!(normalize_search_depth("search_depth", Some("turbo")).is_err());
    }

    /// `is_deep_mode` is the exact-match sibling the routing layer reads after
    /// canonicalization. The leniency must NOT leak into it: a caller that
    /// forgot to canonicalize has to fail loudly, not be rescued here.
    #[test]
    fn is_deep_mode_stays_exact() {
        assert!(is_deep_mode(Some("deep-reasoning")));
        assert!(!is_deep_mode(Some("deep_reasoning")));
        assert!(!is_deep_mode(Some("DEEP-REASONING")));
    }

    /// `time_range` used to be forwarded to the vendors completely unvalidated,
    /// which made `"W"`, `"week"` and `"WEEK "` three different upstream bodies
    /// for one request. The long forms are canonical; Tavily's documented single
    /// letters are accepted input spellings.
    #[test]
    fn time_range_folds_short_forms_and_casing() {
        for (input, want) in [
            ("W", "week"),
            (" w ", "week"),
            ("week", "week"),
            ("WEEK", "week"),
            ("d", "day"),
            ("m", "month"),
            ("y", "year"),
            ("Year", "year"),
        ] {
            assert_eq!(
                normalize_time_range("time_range", Some(input))
                    .unwrap()
                    .as_deref(),
                Some(want),
                "{input:?}"
            );
        }
        for unset in [None, Some(""), Some("   ")] {
            assert_eq!(normalize_time_range("time_range", unset).unwrap(), None);
        }
    }

    /// The one deliberate NEW refusal on this surface, and the edge of the short
    /// form rule: only the four documented letters fold. The error keeps the
    /// shape of every other knob and quotes the raw value.
    #[test]
    fn time_range_rejects_junk_and_unlisted_short_forms() {
        for junk in ["banana", "q", "hour", "weeks", "yesterday", "."] {
            let err = normalize_time_range("time_range", Some(junk))
                .expect_err("an unadvertised time range must be refused");
            assert!(err.contains("time_range"), "{err}");
            assert!(err.contains("is not a supported value"), "{err}");
            assert!(err.contains("day, week, month, year"), "valid set: {err}");
        }
    }

    /// The everyday country spellings `country.rs` added, seen through the
    /// vendor-token path Tavily is dialed with — and the two classes it still
    /// refuses rather than guess.
    #[test]
    fn country_filter_resolves_everyday_spellings_but_not_ambiguity() {
        for (input, token) in [
            ("usa", "united states"),
            ("U.S.", "united states"),
            ("America", "united states"),
            ("UK", "united kingdom"),
            ("Great Britain", "united kingdom"),
            ("GB R", "united kingdom"),
        ] {
            assert_eq!(
                normalize_country_filter(input).as_deref(),
                Some(token),
                "{input:?}"
            );
        }
        // `korea` is AMBIGUOUS (both `north korea` and `south korea` are tokens)
        // and `england`/`scotland`/`wales` would SILENTLY BROADEN to a sovereign
        // state — both are refusals, not leniency gaps.
        for refused in ["korea", "england", "scotland", "wales"] {
            assert_eq!(
                normalize_country_filter(refused),
                None,
                "{refused:?} must not be guessed"
            );
        }
    }

    /// A set whose members differ only by separators is the one case the fold
    /// must refuse instead of picking a winner. Note the guard is only
    /// REACHABLE when a set carries a non-canonical spelling — an exact member
    /// always wins at step 1 — which is precisely the accident the tripwire in
    /// `sets_are_disjoint_and_complete` prevents, so this test uses a synthetic
    /// set (`fast_read` next to `fastread`) to keep the branch honest.
    #[test]
    fn ambiguous_spelling_is_refused_not_guessed() {
        let valid = ["fast_read", "fastread"];
        let err = normalize_choice("depth", Some("FAST READ"), &valid)
            .expect_err("two members, one answer");
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("fast_read"), "candidates named: {err}");
        // An exact member still wins: the client spelled a real value.
        assert_eq!(
            normalize_choice("depth", Some("fastread"), &valid)
                .unwrap()
                .as_deref(),
            Some("fastread")
        );
    }

    /// `include_content` is refused for BOTH values on the deep path: the
    /// deep loop's search legs are hard-coded contentless and its scrapes
    /// hard-coded full, so the knob could not be delivered either way (it was
    /// silently INVERTED). `None` stays legal, and the knob is irrelevant
    /// without `deep` — the standard loop honors the caller's value.
    #[test]
    fn deep_refuses_include_content_for_either_value() {
        for want in [Some(true), Some(false)] {
            let err = validate_deep_research_knobs(true, None, None, Some(0), want)
                .expect_err("deep + includeContent must be refused");
            assert!(err.contains("deep"), "{err}");
            assert!(err.contains("includeContent"), "must name the knob: {err}");
        }
        assert_eq!(
            validate_deep_research_knobs(true, None, None, Some(0), None),
            Ok(()),
            "deep without includeContent must pass"
        );
        for want in [Some(true), Some(false), None] {
            validate_deep_research_knobs(false, Some("tavily"), Some("mla"), Some(5), want)
                .expect("standard research keeps every knob");
        }
    }
}
