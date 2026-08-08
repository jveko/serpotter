//! URL normalization for dedupe / RRF keys (mysearch url-normalize parity).

use std::collections::HashSet;
use std::sync::LazyLock;

static TRACKING_PARAMS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "utm_source",
        "utm_medium",
        "utm_campaign",
        "utm_term",
        "utm_content",
        "utm_id",
        "utm_source_platform",
        "utm_creative_format",
        "utm_marketing_tactic",
        "gclid",
        "gclsrc",
        "dclid",
        "gbraid",
        "wbraid",
        "fbclid",
        "fb_action_ids",
        "fb_action_types",
        "fb_ref",
        "fb_source",
        "msclkid",
        "mc_cid",
        "mc_eid",
        "twclid",
        "li_fat_id",
        "igshid",
        "ttclid",
        "ref",
        "source",
        "via",
        "share",
        "_ga",
        "_gl",
        "ck_subscriber_id",
        "oly_enc_id",
        "oly_anon_id",
        "si",
        "feature",
        "app",
    ])
});

/// Normalize URL: lowercase host, strip www, fragment, tracking params, trailing slash.
pub fn normalize_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_lowercase().trim_end_matches('/').to_string();
    };

    parsed.set_fragment(None);
    if let Some(host) = parsed.host_str().map(|h| h.to_lowercase()) {
        let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
        let _ = parsed.set_host(Some(&host));
    }
    let _ = parsed.set_scheme(&parsed.scheme().to_lowercase());

    // Filter query pairs
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.contains(k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.is_empty() {
        parsed.set_query(None);
    } else {
        // Re-encode each key/value via form_urlencoded instead of re-joining the
        // raw (already percent-decoded) pairs, so reserved chars (& = + / % space)
        // cannot produce a malformed or ambiguous query string. This keeps the
        // normalized key canonical: distinct decoded values stay distinct, equal
        // decoded values merge (e.g. a%2Fb and a/b).
        let q = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs)
            .finish();
        parsed.set_query(Some(&q));
    }

    // Prefer path without trailing slash (except bare root).
    {
        let path = parsed.path().to_string();
        if path.len() > 1 && path.ends_with('/') {
            parsed.set_path(path.trim_end_matches('/'));
        }
    }
    parsed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_utm_and_www() {
        let a = normalize_url("https://WWW.Example.com/path/?utm_source=x&id=1");
        let b = normalize_url("https://example.com/path?id=1");
        assert_eq!(a, b);
    }

    #[test]
    fn encoded_query_values_stay_distinct() {
        let a = normalize_url("https://example.com/?q=a%2Fb");
        let b = normalize_url("https://example.com/?q=a%2Fc");
        assert_ne!(a, b, "distinct encoded values must not collapse");
        assert_eq!(a, "https://example.com/?q=a%2Fb");
        assert_eq!(b, "https://example.com/?q=a%2Fc");
    }

    #[test]
    fn encoded_and_raw_slashes_merge() {
        // a%2Fb decodes to a/b, so both spellings must produce the same key.
        assert_eq!(
            normalize_url("https://example.com/?q=a/b"),
            normalize_url("https://example.com/?q=a%2Fb")
        );
    }

    #[test]
    fn reserved_chars_in_values_are_reencoded() {
        // '+' and '%20' both decode to a space; both must canonically re-encode
        // to '+' rather than splicing a raw space / '&' / '=' into the query.
        let a = normalize_url("https://example.com/?q=a+b&x=y");
        let b = normalize_url("https://example.com/?q=a%20b&x=y");
        assert_eq!(a, b);
        assert_eq!(a, "https://example.com/?q=a+b&x=y");
        assert_eq!(
            normalize_url("https://example.com/?q=a%26x%3Dy"),
            "https://example.com/?q=a%26x%3Dy"
        );
    }

    #[test]
    fn tracking_params_stripped_even_with_encoded_values() {
        assert_eq!(
            normalize_url("https://example.com/p?utm_source=x&q=a%2Fb&gclid=1"),
            normalize_url("https://example.com/p?q=a%2Fb")
        );
    }
}
