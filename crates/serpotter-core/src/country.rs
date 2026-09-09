//! Country tokens the search vendors accept, paired with ISO-3166 alpha-2
//! codes - the single authority for the `country` search filter.
//!
//! The VALUES are Tavily's documented CLOSED enum of lowercase country names,
//! extracted verbatim from its API reference (`country.enum`, 166 tokens,
//! afghanistan..zimbabwe) on 2026-09-09:
//! https://docs.tavily.com/documentation/api-reference/endpoint/search - the
//! same URL Tavily's own `400 Invalid country` message points at. They are NOT
//! the ISO 3166-1 English short names, and the difference is load-bearing:
//! Tavily takes `czech republic`, `vietnam`, `turkey`, `russia`,
//! `south korea` where ISO says `Czechia`, `Viet Nam`, `Türkiye`, `Russian
//! Federation`, `Korea, Republic of`. Keying the table on ISO would make us
//! forward a name the vendor rejects (a guaranteed 400) and locally refuse a
//! name it accepts (a client-visible regression).
//!
//! Anything outside this enum is refused locally: a country Tavily does not
//! list (`LA`, `VA`, `PS`, `HK`, `AQ`, ...) resolves to `None`, so the provider
//! layer returns a non-retryable 400 instead of burning a key on a vendor 400.
//! ISO spellings are kept as INPUT aliases (`COUNTRY_ALIASES` below) so
//! `Czechia`/`Türkiye`/`Korea, Republic of` still resolve to the vendor token,
//! and the everyday spellings that are not ISO names at all (`usa`, `UK`,
//! `Great Britain`) in `COUNTRY_COMMON_ALIASES`.
//!
//! Refresh: re-fetch the page, take the `country.enum` bullets verbatim, and
//! pair each token with its ISO-3166 alpha-2 by fold-matching it against that
//! code's `name`/`common_name`/`official_name`. The handful of vendor common
//! names with no ISO spelling to fold onto are paired by hand: `brunei`/bn,
//! `cape verde`/cv, `russia`/ru, `turkey`/tr. Codes with no token stay OUT of
//! the table on purpose. Update the token blob and both alias blobs (and
//! `COUNTRY_ENTRY_COUNT` when the enum itself changes); the tests below fail
//! loudly on a dropped segment or a shadowed name.

/// `"code token"` pairs joined by `|`: ISO-3166 alpha-2 (lowercase,
/// alphabetical) and the exact lowercase vendor token to send. One `concat!`
/// blob keeps the table reviewable and lets a lookup borrow straight out of it,
/// so no lazily-built map is needed.
const COUNTRY_TOKENS: &str = concat!(
    "ad andorra|ae united arab emirates|af afghanistan|al albania|am armenia|ao angola",
    "|",
    "ar argentina|at austria|au australia|az azerbaijan|ba bosnia and herzegovina|bb barbados",
    "|",
    "bd bangladesh|be belgium|bf burkina faso|bg bulgaria|bh bahrain|bi burundi",
    "|",
    "bj benin|bn brunei|bo bolivia|br brazil|bs bahamas|bt bhutan",
    "|",
    "bw botswana|by belarus|bz belize|ca canada|cf central african republic|cg congo",
    "|",
    "ch switzerland|cl chile|cm cameroon|cn china|co colombia|cr costa rica",
    "|",
    "cu cuba|cv cape verde|cy cyprus|cz czech republic|de germany|dj djibouti",
    "|",
    "dk denmark|do dominican republic|dz algeria|ec ecuador|ee estonia|eg egypt",
    "|",
    "er eritrea|es spain|et ethiopia|fi finland|fj fiji|fr france",
    "|",
    "ga gabon|gb united kingdom|ge georgia|gh ghana|gm gambia|gn guinea",
    "|",
    "gq equatorial guinea|gr greece|gt guatemala|hn honduras|hr croatia|ht haiti",
    "|",
    "hu hungary|id indonesia|ie ireland|il israel|in india|iq iraq",
    "|",
    "ir iran|is iceland|it italy|jm jamaica|jo jordan|jp japan",
    "|",
    "ke kenya|kg kyrgyzstan|kh cambodia|km comoros|kp north korea|kr south korea",
    "|",
    "kw kuwait|kz kazakhstan|lb lebanon|li liechtenstein|lk sri lanka|lr liberia",
    "|",
    "ls lesotho|lt lithuania|lu luxembourg|lv latvia|ly libya|ma morocco",
    "|",
    "mc monaco|md moldova|me montenegro|mg madagascar|mk north macedonia|ml mali",
    "|",
    "mm myanmar|mn mongolia|mr mauritania|mt malta|mu mauritius|mv maldives",
    "|",
    "mw malawi|mx mexico|my malaysia|mz mozambique|na namibia|ne niger",
    "|",
    "ng nigeria|ni nicaragua|nl netherlands|no norway|np nepal|nz new zealand",
    "|",
    "om oman|pa panama|pe peru|pg papua new guinea|ph philippines|pk pakistan",
    "|",
    "pl poland|pt portugal|py paraguay|qa qatar|ro romania|rs serbia",
    "|",
    "ru russia|rw rwanda|sa saudi arabia|sd sudan|se sweden|sg singapore",
    "|",
    "si slovenia|sk slovakia|sn senegal|so somalia|ss south sudan|sv el salvador",
    "|",
    "sy syria|td chad|tg togo|th thailand|tj tajikistan|tm turkmenistan",
    "|",
    "tn tunisia|tr turkey|tt trinidad and tobago|tw taiwan|tz tanzania|ua ukraine",
    "|",
    "ug uganda|us united states|uy uruguay|uz uzbekistan|ve venezuela|vn vietnam",
    "|",
    "ye yemen|za south africa|zm zambia|zw zimbabwe",
);

/// `"alias code"` pairs joined by `|`: ISO-documented spellings (official /
/// common names, in either word order) mapped to the code whose vendor token we
/// should send instead. Input-only - a lookup returns the canonical token, never
/// the alias. Ambiguous spellings were dropped at generation time.
const COUNTRY_ALIASES: &str = concat!(
    "Principality of Andorra ad|Islamic Republic of Afghanistan af|Republic of Albania al|Republic of Armenia am|Republic of Angola ao|Argentine Republic ar",
    "|",
    "Republic of Austria at|Republic of Azerbaijan az|Republic of Bosnia and Herzegovina ba|People's Republic of Bangladesh bd|Kingdom of Belgium be|Republic of Bulgaria bg",
    "|",
    "Kingdom of Bahrain bh|Republic of Burundi bi|Republic of Benin bj|Brunei Darussalam bn|Bolivia, Plurinational State of bo|Plurinational State of Bolivia bo",
    "|",
    "Federative Republic of Brazil br|Commonwealth of the Bahamas bs|Kingdom of Bhutan bt|Republic of Botswana bw|Republic of Belarus by|Republic of the Congo cg",
    "|",
    "Swiss Confederation ch|Republic of Chile cl|Republic of Cameroon cm|People's Republic of China cn|Republic of Colombia co|Republic of Costa Rica cr",
    "|",
    "Republic of Cuba cu|Cabo Verde cv|Republic of Cabo Verde cv|Republic of Cyprus cy|Czechia cz|Federal Republic of Germany de",
    "|",
    "Republic of Djibouti dj|Kingdom of Denmark dk|People's Democratic Republic of Algeria dz|Republic of Ecuador ec|Republic of Estonia ee|Arab Republic of Egypt eg",
    "|",
    "the State of Eritrea er|Kingdom of Spain es|Federal Democratic Republic of Ethiopia et|Republic of Finland fi|Republic of Fiji fj|French Republic fr",
    "|",
    "Gabonese Republic ga|United Kingdom of Great Britain and Northern Ireland gb|Republic of Ghana gh|Republic of the Gambia gm|Republic of Guinea gn|Republic of Equatorial Guinea gq",
    "|",
    "Hellenic Republic gr|Republic of Guatemala gt|Republic of Honduras hn|Republic of Croatia hr|Republic of Haiti ht|Republic of Indonesia id",
    "|",
    "State of Israel il|Republic of India in|Republic of Iraq iq|Iran, Islamic Republic of ir|Islamic Republic of Iran ir|Republic of Iceland is",
    "|",
    "Italian Republic it|Hashemite Kingdom of Jordan jo|Republic of Kenya ke|Kyrgyz Republic kg|Kingdom of Cambodia kh|Union of the Comoros km",
    "|",
    "Democratic People's Republic of Korea kp|Korea, Democratic People's Republic of kp|Korea, Republic of kr|Republic of Korea kr|State of Kuwait kw|Republic of Kazakhstan kz",
    "|",
    "Lebanese Republic lb|Principality of Liechtenstein li|Democratic Socialist Republic of Sri Lanka lk|Republic of Liberia lr|Kingdom of Lesotho ls|Republic of Lithuania lt",
    "|",
    "Grand Duchy of Luxembourg lu|Republic of Latvia lv|Kingdom of Morocco ma|Principality of Monaco mc|Moldova, Republic of md|Republic of Moldova md",
    "|",
    "Republic of Madagascar mg|Republic of North Macedonia mk|Republic of Mali ml|Republic of Myanmar mm|Islamic Republic of Mauritania mr|Republic of Malta mt",
    "|",
    "Republic of Mauritius mu|Republic of Maldives mv|Republic of Malawi mw|United Mexican States mx|Republic of Mozambique mz|Republic of Namibia na",
    "|",
    "Republic of the Niger ne|Federal Republic of Nigeria ng|Republic of Nicaragua ni|Kingdom of the Netherlands nl|Kingdom of Norway no|Federal Democratic Republic of Nepal np",
    "|",
    "Sultanate of Oman om|Republic of Panama pa|Republic of Peru pe|Independent State of Papua New Guinea pg|Republic of the Philippines ph|Islamic Republic of Pakistan pk",
    "|",
    "Republic of Poland pl|Portuguese Republic pt|Republic of Paraguay py|State of Qatar qa|Republic of Serbia rs|Russian Federation ru",
    "|",
    "Rwandese Republic rw|Kingdom of Saudi Arabia sa|Republic of the Sudan sd|Kingdom of Sweden se|Republic of Singapore sg|Republic of Slovenia si",
    "|",
    "Slovak Republic sk|Republic of Senegal sn|Federal Republic of Somalia so|Republic of South Sudan ss|Republic of El Salvador sv|Syrian Arab Republic sy",
    "|",
    "Republic of Chad td|Togolese Republic tg|Kingdom of Thailand th|Republic of Tajikistan tj|Republic of Tunisia tn|Republic of Türkiye tr",
    "|",
    "Türkiye tr|Republic of Trinidad and Tobago tt|Province of China Taiwan tw|Taiwan, Province of China tw|Tanzania, United Republic of tz|United Republic of Tanzania tz",
    "|",
    "Republic of Uganda ug|United States of America us|Eastern Republic of Uruguay uy|Republic of Uzbekistan uz|Bolivarian Republic of Venezuela ve|Venezuela, Bolivarian Republic of ve",
    "|",
    "Socialist Republic of Viet Nam vn|Republic of Yemen ye|Republic of South Africa za|Republic of Zambia zm|Republic of Zimbabwe zw",
);

/// `"alias code"` pairs for the EVERYDAY spellings that are not ISO names at
/// all: `usa`/`america`/`u.s.` for the United States, `UK`/`GB`/`Great Britain`
/// for the United Kingdom. Same shape and same fold as `COUNTRY_ALIASES`, kept
/// in its own blob because it is consulted by exactly ONE direction —
/// [`canonical_country_name`] (and so `normalize_country_filter`, the token
/// Tavily is sent) — and deliberately NOT by [`country_code_by_name`].
///
/// The asymmetry is load-bearing, not tidiness. `country_code_by_name` answers
/// "is this one of the enum's NAMES", and Firecrawl documents `UK` as one of
/// its own country-code examples while `UK` sits outside ISO-3166 (this table
/// has `gb united kingdom` and no `uk` code) — which is why
/// `providers/src/firecrawl.rs` passes any value the table cannot prove
/// through verbatim. Resolving `uk` in the name→code direction would turn a
/// working Firecrawl `UK` filter into `GB` without anyone asking.
///
/// Only UNAMBIGUOUS spellings belong here. `korea` is absent (both `north
/// korea` and `south korea` are in the enum, so picking one is a wrong filter,
/// not a loose one) and so are `england`/`scotland`/`wales` (the vendor has no
/// token for them; answering `united kingdom` would silently broaden the
/// results to a sovereign state the client did not ask for).
const COUNTRY_COMMON_ALIASES: &str =
    "usa us|u.s. us|u.s.a. us|america us|uk gb|gb gb|gb r gb|great britain gb";

/// Size of the vendor enum this table was extracted from. The well-formedness
/// test pins the TOKEN blob to it — the two alias blobs are input-only and are
/// checked for shape, folded-key uniqueness and a resolvable code instead — so
/// a dropped or duplicated `|` segment fails the build rather than silently
/// shrinking the accepted country set.
#[cfg(test)]
const COUNTRY_ENTRY_COUNT: usize = 166;

/// Canonical vendor token for an ISO-3166 alpha-2 code (`id` and `ID` both give
/// `indonesia`), or `None` when the code is not a two-letter code or is not in
/// the enum (a country this vendor cannot filter on).
///
/// EXACT code equality, never a prefix test: `uk ukraine` and `gb united
/// kingdom` share a two-letter prefix, so a `starts_with` scan could answer one
/// for the other. Entries split at the FIRST space (code, then the token).
pub fn country_name(iso2: &str) -> Option<&'static str> {
    let query = iso2.to_ascii_lowercase();
    let code = query.as_bytes();
    if code.len() != 2 || !code.iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    COUNTRY_TOKENS.split('|').find_map(|entry| {
        let (entry_code, token) = entry.split_once(' ')?;
        (entry_code == query).then_some(token)
    })
}

/// Canonical (lowercase, vendor-speak) country NAME: `united states`,
/// `South Korea`, `Czechia` and `Korea, Republic of` all resolve to the token
/// we can actually send; `None` for anything outside the enum - which is
/// exactly what the vendor would 400 on (`Nonsenseland`, `Palestine`,
/// `["Indonesia"]`).
///
/// Matching is case-, accent- and whitespace-insensitive, over the vendor
/// tokens first, the ISO input aliases second and the everyday spellings last.
/// Alpha-2 CODES are the public job of [`country_name`]; where a code is also a
/// common alias (`gb`) both paths answer the same token, and the one that is
/// not a code at all (`uk`) exists only here — so the lookups still cannot
/// disagree.
pub fn canonical_country_name(input: &str) -> Option<&'static str> {
    if let Some(token) = find_token(input) {
        return Some(token);
    }
    let code = find_alias_code(input).or_else(|| find_common_alias_code(input))?;
    country_name(code)
}

/// Inverse lookup: a country NAME (vendor token, ISO alias, any case/accent
/// spelling) -> its ISO-3166 alpha-2 code, lowercase (`indonesia` and
/// `South Korea` both give `id`/`kr`). Vendors disagree about the `country`
/// dialect - Firecrawl wants the code, Tavily the name - and this reads the
/// same table the other way; `None` when the name is outside the enum.
///
/// Matches the WHOLE folded name (`united states` never matches
/// `united arab emirates`), never a prefix or a substring — and over the vendor
/// tokens plus the ISO aliases ONLY. Everyday abbreviations (`uk`, `usa`) are
/// not country names: they resolve through [`canonical_country_name`] for the
/// name→TOKEN direction and stay `None` here, which is what keeps a
/// Firecrawl-documented `UK` from being rewritten to `GB` on that vendor's wire.
pub fn country_code_by_name(name: &str) -> Option<&'static str> {
    COUNTRY_TOKENS
        .split('|')
        .find_map(|entry| {
            let (code, token) = entry.split_once(' ')?;
            name_matches(name, token).then_some(code)
        })
        .or_else(|| find_alias_code(name))
}

fn find_token(input: &str) -> Option<&'static str> {
    COUNTRY_TOKENS.split('|').find_map(|entry| {
        let (_, token) = entry.split_once(' ')?;
        name_matches(input, token).then_some(token)
    })
}

fn find_alias_code(input: &str) -> Option<&'static str> {
    COUNTRY_ALIASES.split('|').find_map(|entry| {
        // Aliases contain spaces, so the code is the LAST field.
        let (alias, code) = entry.rsplit_once(' ')?;
        name_matches(input, alias).then_some(code)
    })
}

/// Lookup for [`COUNTRY_COMMON_ALIASES`]; same `"alias code"` tail split as
/// [`find_alias_code`].
fn find_common_alias_code(input: &str) -> Option<&'static str> {
    COUNTRY_COMMON_ALIASES.split('|').find_map(|entry| {
        let (alias, code) = entry.rsplit_once(' ')?;
        name_matches(input, alias).then_some(code)
    })
}

/// True when `query` names the same country as `canonical` modulo case, the
/// accents this table carries, and whitespace placement - so `vietnam` matches
/// `Viet Nam`, `turkiye` and `Türkiye` both match `turkey`, and `SOUTH KOREA`
/// matches `south korea`.
fn name_matches(query: &str, canonical: &str) -> bool {
    query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(fold_country_char)
        .eq(canonical
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(fold_country_char))
}

/// One-character fold for [`name_matches`]: lowercase, then replace the
/// diacritics this table actually carries with their ASCII base letter and the
/// typographic apostrophe with the ASCII one, so ASCII-typed inputs resolve.
/// The `fold_map_covers_every_non_ascii_char` test below is what keeps this map
/// complete as the table is refreshed.
fn fold_country_char(c: char) -> char {
    match c.to_lowercase().next().unwrap_or(c) {
        'à' => 'a',
        'á' => 'a',
        'ã' => 'a',
        'å' => 'a',
        'ç' => 'c',
        'è' => 'e',
        'é' => 'e',
        'ê' => 'e',
        'ë' => 'e',
        'í' => 'i',
        'î' => 'i',
        'ï' => 'i',
        'ñ' => 'n',
        'ò' => 'o',
        'ó' => 'o',
        'ô' => 'o',
        'õ' => 'o',
        'ö' => 'o',
        'ø' => 'o',
        'ù' => 'u',
        'ú' => 'u',
        'û' => 'u',
        'ü' => 'u',
        'ý' => 'y',
        'ć' => 'c',
        'č' => 'c',
        'đ' => 'd',
        'ě' => 'e',
        'ğ' => 'g',
        'ı' => 'i',
        'ĺ' => 'l',
        'ł' => 'l',
        'ń' => 'n',
        'ŕ' => 'r',
        'ś' => 's',
        'ş' => 's',
        'š' => 's',
        'ť' => 't',
        'ž' => 'z',
        'ț' => 't',
        '’' => '\'',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn country_name_accepts_either_code_case() {
        assert_eq!(country_name("id"), Some("indonesia"));
        assert_eq!(country_name("ID"), Some("indonesia"));
        assert_eq!(country_name("us"), Some("united states"));
        assert_eq!(country_name("GB"), Some("united kingdom"));
        assert_eq!(country_name("ae"), Some("united arab emirates"));
    }

    /// The reason this table is keyed on Tavily's enum and not on ISO: these are
    /// the spellings where the two disagree, so a regression here sends a name
    /// Tavily 400s on.
    #[test]
    fn vendor_tokens_beat_iso_short_names() {
        for (code, token) in [
            ("cz", "czech republic"),
            ("kr", "south korea"),
            ("kp", "north korea"),
            ("vn", "vietnam"),
            ("tr", "turkey"),
            ("ru", "russia"),
            ("mk", "north macedonia"),
            ("md", "moldova"),
            ("cg", "congo"),
            ("bf", "burkina faso"),
            ("cf", "central african republic"),
            ("bs", "bahamas"),
            ("ir", "iran"),
            ("sy", "syria"),
            ("ve", "venezuela"),
        ] {
            assert_eq!(country_name(code), Some(token), "{code} token");
        }
    }

    #[test]
    fn countries_outside_the_vendor_enum_resolve_to_none() {
        // Tavily's enum omits these; refusing locally beats a vendor 400.
        for code in [
            "va", "xk", "aq", "ci", "la", "sz", "ps", "hk", "mo", "zz", "i", "",
        ] {
            assert_eq!(country_name(code), None, "{code} is not in the enum");
        }
    }

    #[test]
    fn name_input_canonicalizes_to_the_vendor_token() {
        for (input, token) in [
            ("indonesia", "indonesia"),
            ("Indonesia", "indonesia"),
            ("South Korea", "south korea"),
            ("SOUTH KOREA", "south korea"),
            ("united states", "united states"),
            ("viet nam", "vietnam"),
            ("czechia", "czech republic"),
            ("Korea, Republic of", "south korea"),
            ("Russian Federation", "russia"),
            ("Brunei Darussalam", "brunei"),
            ("Cabo Verde", "cape verde"),
            // The ISO official name is an accepted INPUT alias; what we send is
            // still always the vendor token.
            ("United States of America", "united states"),
        ] {
            assert_eq!(
                canonical_country_name(input),
                Some(token),
                "{input:?} must canonicalize"
            );
        }
    }

    /// Prefix/near-miss hazards in the enum: `uk ukraine` vs `gb united
    /// kingdom`, and `united states` vs `united arab emirates`. A scan that
    /// matched on a prefix would answer these wrongly and silently.
    #[test]
    fn near_miss_codes_and_names_do_not_shadow_each_other() {
        // The name->CODE direction must keep refusing the everyday spellings
        // (see `common_aliases_do_not_leak_into_the_inverse_lookup`), and the
        // name->token direction must answer UNITED KINGDOM, never Ukraine.
        assert_eq!(country_code_by_name("uk"), None, "uk is not a country name");
        assert_eq!(country_code_by_name("ua"), None, "ua is a code, not a name");
        assert_eq!(canonical_country_name("uk"), Some("united kingdom"));
        assert_eq!(canonical_country_name("UKRAINE"), Some("ukraine"));
        assert_eq!(country_code_by_name("ukraine"), Some("ua"));
        assert_eq!(country_code_by_name("united kingdom"), Some("gb"));
        assert_eq!(country_code_by_name("united states"), Some("us"));
        assert_eq!(country_code_by_name("united arab emirates"), Some("ae"));
        assert_eq!(country_code_by_name("united"), None, "partial name");
        assert_eq!(country_name("GB"), Some("united kingdom"));
        assert_eq!(country_name("gb"), Some("united kingdom"));
        assert_eq!(country_name("UK"), None, "UK is not an ISO alpha-2 code");
        assert_eq!(country_name("ua"), Some("ukraine"));
        assert_eq!(country_name("ae"), Some("united arab emirates"));
        assert_eq!(country_name("us"), Some("united states"));
    }

    #[test]
    fn accent_and_apostrophe_folds_resolve() {
        // The table's only diacritic today is u-umlaut (Turkiye); the fold map
        // still covers the rest so a refresh cannot silently break matching.
        assert_eq!(canonical_country_name("Turkiye"), Some("turkey"));
        assert_eq!(canonical_country_name("TURKIYE"), Some("turkey"));
        assert_eq!(canonical_country_name("turkey"), Some("turkey"));
        assert_eq!(
            canonical_country_name("republic of turkiye"),
            Some("turkey")
        );
        assert_eq!(canonical_country_name("laos"), None);
        assert_eq!(canonical_country_name("Cote d'Ivoire"), None);
    }

    #[test]
    fn names_outside_the_enum_are_refused() {
        // `Great Britain` is in this list no longer: it is an accepted everyday
        // alias now (see `everyday_spellings_resolve_to_their_vendor_token`),
        // while `england` and `korea` stay refused for the reasons pinned there.
        for junk in [
            "",
            "   ",
            "Nonsenseland",
            "Palestine",
            "Kosovo",
            "Ivory Coast",
            "South Korea!",
            r#"["Indonesia"]"#,
        ] {
            assert_eq!(
                canonical_country_name(junk),
                None,
                "{junk:?} must not reach a vendor"
            );
        }
    }

    #[test]
    fn country_code_by_name_inverts_the_table() {
        assert_eq!(country_code_by_name("Indonesia"), Some("id"));
        assert_eq!(country_code_by_name("indonesia"), Some("id"));
        assert_eq!(country_code_by_name("united states"), Some("us"));
        assert_eq!(country_code_by_name("South Korea"), Some("kr"));
        assert_eq!(country_code_by_name("Czechia"), Some("cz"));
        assert_eq!(country_code_by_name("Korea, Republic of"), Some("kr"));
        assert!(country_code_by_name("Nonsenseland").is_none());
    }

    /// Every vendor token must be sendable as-is: lowercase ASCII words, single
    /// spaces. A bad paste into the blob fails here rather than 400ing later.
    #[test]
    fn tokens_are_lowercase_ascii_words() {
        for entry in COUNTRY_TOKENS.split('|') {
            let (code, token) = entry
                .split_once(' ')
                .unwrap_or_else(|| panic!("{entry:?} is not `code token`"));
            assert_eq!(code.len(), 2, "{entry:?}");
            assert!(
                token.bytes().all(|b| b == b' ' || b.is_ascii_lowercase()),
                "{token:?} must be lowercase ASCII letters and spaces"
            );
            assert!(
                !token.starts_with(' ') && !token.ends_with(' '),
                "{entry:?}"
            );
            assert!(!token.contains("  "), "{entry:?} double space");
        }
    }

    /// Parses both blobs and round-trips all three lookups over every row: this
    /// is what proves no `|` segment was dropped or duplicated and that no alias
    /// shadows another country's name.
    #[test]
    fn whole_table_is_well_formed_and_unambiguous() {
        let tokens: Vec<&str> = COUNTRY_TOKENS.split('|').collect();
        assert_eq!(tokens.len(), COUNTRY_ENTRY_COUNT, "vendor enum size");
        let aliases: Vec<&str> = COUNTRY_ALIASES.split('|').collect();
        assert!(!aliases.is_empty(), "ISO input aliases must exist");

        let mut codes: Vec<&str> = Vec::with_capacity(tokens.len());
        let mut names: Vec<&str> = Vec::with_capacity(tokens.len());
        for (idx, entry) in tokens.iter().enumerate() {
            let (code, token) = entry
                .split_once(' ')
                .unwrap_or_else(|| panic!("token {idx} {entry:?} has no code+token shape"));
            assert!(
                code.bytes().all(|b| b.is_ascii_lowercase()),
                "{entry:?} code must be lowercase"
            );
            assert_eq!(Some(token), country_name(code), "{entry:?}");
            assert_eq!(Some(token), country_name(&code.to_uppercase()), "{entry:?}");
            assert_eq!(
                Some(token),
                canonical_country_name(token),
                "{entry:?} token lookup must be unambiguous"
            );
            assert_eq!(
                Some(code),
                country_code_by_name(token),
                "{entry:?} inverse lookup"
            );
            codes.push(code);
            names.push(token);
        }
        assert_eq!(
            codes.len(),
            codes
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "codes must be unique"
        );
        assert_eq!(
            names.len(),
            names
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "tokens must be unique"
        );
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "codes must stay alphabetical");

        let mut alias_keys: Vec<String> = Vec::with_capacity(aliases.len());
        for (idx, entry) in aliases.iter().enumerate() {
            let (alias, code) = entry
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("alias {idx} {entry:?} has no alias+code shape"));
            assert_eq!(code.len(), 2, "{entry:?}");
            assert!(
                codes.contains(&code),
                "{entry:?} points at an unusable code"
            );
            // An alias must resolve to its own country through both directions.
            assert_eq!(
                country_name(code),
                canonical_country_name(alias),
                "{alias:?} must canonicalize to {code}"
            );
            assert_eq!(
                Some(code),
                country_code_by_name(alias),
                "{alias:?} must invert to {code}"
            );
            alias_keys.push(
                alias
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .map(fold_country_char)
                    .collect(),
            );
        }
        assert_eq!(
            alias_keys.len(),
            alias_keys
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "alias match keys must be unique"
        );
    }

    #[test]
    fn fold_map_covers_every_non_ascii_char() {
        // Whichever diacritics the enum, the ISO aliases or the everyday
        // aliases carry must fold, or ASCII-typed clients stop resolving; the
        // map is complete today.
        for c in COUNTRY_TOKENS
            .chars()
            .chain(COUNTRY_ALIASES.chars())
            .chain(COUNTRY_COMMON_ALIASES.chars())
            .filter(|c| !c.is_ascii())
        {
            assert_ne!(fold_country_char(c), c, "fold map is missing {c:?}");
        }
    }

    /// The spellings a client actually types that are neither an ISO name nor a
    /// vendor token. Each resolves to the VENDOR's token, never to the string
    /// the client sent.
    #[test]
    fn everyday_spellings_resolve_to_their_vendor_token() {
        for (input, token) in [
            ("usa", "united states"),
            ("USA", "united states"),
            (" u.s. ", "united states"),
            ("U.S.A.", "united states"),
            ("america", "united states"),
            ("United States of America", "united states"),
            ("uk", "united kingdom"),
            ("UK", "united kingdom"),
            ("gb", "united kingdom"),
            ("GB R", "united kingdom"),
            ("Great Britain", "united kingdom"),
            (
                "United Kingdom of Great Britain and Northern Ireland",
                "united kingdom",
            ),
        ] {
            assert_eq!(
                canonical_country_name(input),
                Some(token),
                "{input:?} must resolve to {token:?}"
            );
        }
    }

    /// The two classes the leniency deliberately does NOT cover, pinned so a
    /// future "helpful" alias cannot quietly add them.
    #[test]
    fn ambiguous_and_broadening_spellings_stay_refused() {
        // AMBIGUOUS: both `north korea` and `south korea` are vendor tokens, so
        // answering either one for `korea` is a wrong filter, not a looser one.
        assert_eq!(canonical_country_name("korea"), None);
        assert_eq!(canonical_country_name("Korea"), None);
        // SILENT BROADENING: the enum has no token for a home nation, and
        // mapping one to `united kingdom` would return a sovereign state the
        // client never asked for.
        for name in ["england", "scotland", "wales"] {
            assert_eq!(canonical_country_name(name), None, "{name:?}");
        }
    }

    /// The everyday blob feeds ONE direction. `country_code_by_name` must keep
    /// answering `None` for these: Firecrawl documents `UK` as one of its own
    /// country-code examples and passes any value this table cannot prove
    /// through verbatim, so resolving `uk` here would rewrite a working `UK`
    /// filter into `GB` on that vendor's wire.
    #[test]
    fn common_aliases_do_not_leak_into_the_inverse_lookup() {
        for alias in [
            "usa",
            "u.s.",
            "america",
            "uk",
            "gb",
            "gb r",
            "great britain",
        ] {
            assert_eq!(country_code_by_name(alias), None, "{alias:?}");
        }
    }

    /// Same discipline as `whole_table_is_well_formed_and_unambiguous`, for the
    /// everyday rows: each points at a usable code, resolves to that code's
    /// token, and shadows neither a vendor token nor an ISO alias — otherwise
    /// the lookup order would decide an answer nobody reviewed.
    #[test]
    fn common_alias_table_is_well_formed_and_unshadowed() {
        // `fn` items, not closures: a closure held in a `let` gets its return
        // lifetime as a free inference variable (`for<'a> Fn(&'a str) -> &'b str`),
        // which rustc refuses to tie back to the argument. A nested `fn` elides
        // output-from-input the normal way, so the borrow links correctly.
        fn token_of(entry: &str) -> &str {
            entry.split_once(' ').map_or("", |(_, token)| token)
        }
        fn iso_alias_of(entry: &str) -> &str {
            entry.rsplit_once(' ').map_or("", |(alias, _)| alias)
        }
        let mut seen: Vec<&str> = Vec::new();
        for entry in COUNTRY_COMMON_ALIASES.split('|') {
            let (alias, code) = entry
                .rsplit_once(' ')
                .unwrap_or_else(|| panic!("{entry:?} is not an `alias code` pair"));
            assert_eq!(code.len(), 2, "{entry:?}");
            assert!(
                country_name(code).is_some(),
                "{entry:?} points at a code with no vendor token"
            );
            assert_eq!(
                country_name(code),
                canonical_country_name(alias),
                "{alias:?} must canonicalize to {code}'s token"
            );
            assert!(
                COUNTRY_TOKENS
                    .split('|')
                    .all(|row| !name_matches(alias, token_of(row))),
                "{alias:?} shadows a vendor token"
            );
            assert!(
                COUNTRY_ALIASES
                    .split('|')
                    .all(|row| !name_matches(alias, iso_alias_of(row))),
                "{alias:?} duplicates an ISO alias"
            );
            assert!(
                seen.iter().all(|previous| !name_matches(alias, previous)),
                "{alias:?} duplicates another everyday alias"
            );
            seen.push(alias);
        }
    }
}
