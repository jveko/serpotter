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
    let _ = parsed.set_scheme(
        &parsed.scheme().to_lowercase(),
    );

    // Filter query pairs
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.contains(k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs.is_empty() {
        parsed.set_query(None);
    } else {
        let q = pairs
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("&");
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
}
