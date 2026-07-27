//! Pure mappers for research orchestration.

use crate::dto::ScrapedPage;

/// Web provider stays first for request_log `.first()`; extras append unique only.
pub fn merge_providers_consulted(
    web: String,
    social: Option<String>,
    scrape_providers: impl IntoIterator<Item = String>,
) -> Vec<String> {
    let mut out = vec![web];
    if let Some(s) = social {
        if !out.iter().any(|p| p == &s) {
            out.push(s);
        }
    }
    for sp in scrape_providers {
        if !out.iter().any(|p| p == &sp) {
            out.push(sp);
        }
    }
    out
}

/// Top N scrapable hits: non-empty URL first, then take(n).
pub fn select_scrape_targets(
    items: &[serpotter_core::SearchItem],
    extract_n: usize,
) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|item| !item.url.is_empty())
        .take(extract_n)
        .map(|item| (item.url.clone(), item.title.clone()))
        .collect()
}

/// Map extract success into ScrapedPage; full content only when include_content.
pub fn scraped_page_from_extract(
    title: Option<String>,
    url: String,
    content: String,
    include_content: bool,
) -> ScrapedPage {
    let excerpt = content.chars().take(280).collect::<String>();
    ScrapedPage {
        title,
        url,
        content: if include_content {
            Some(content)
        } else {
            None
        },
        excerpt: Some(excerpt),
        error: None,
    }
}

/// Decide social leg outcome without I/O.
/// `provider_result`: Ok(items) / Err(()) from xAI attempt; ignored when leg skipped.
pub fn map_social_leg(
    social_max_results: Option<u32>,
    social_enabled: bool,
    provider_result: Option<Result<Vec<serpotter_core::SearchItem>, ()>>,
) -> Option<Vec<serpotter_core::SearchItem>> {
    let n = social_max_results.unwrap_or(0);
    if n == 0 || !social_enabled {
        return None; // skip leg
    }
    match provider_result {
        Some(Ok(items)) => Some(items),
        Some(Err(())) | None => Some(Vec::new()), // soft-empty
    }
}

#[cfg(test)]
mod social_leg_tests {
    use super::map_social_leg;

    #[test]
    fn skip_when_zero_or_disabled() {
        assert!(map_social_leg(None, true, Some(Ok(vec![]))).is_none());
        assert!(map_social_leg(Some(0), true, Some(Ok(vec![]))).is_none());
        assert!(map_social_leg(Some(3), false, Some(Ok(vec![]))).is_none());
    }

    #[test]
    fn soft_empty_on_provider_error() {
        let out = map_social_leg(Some(3), true, Some(Err(())));
        assert_eq!(out.as_ref().map(|v| v.len()), Some(0));
    }

    #[test]
    fn soft_empty_when_provider_not_run() {
        // defensive: enabled+n>0 but no result supplied
        let out = map_social_leg(Some(2), true, None);
        assert_eq!(out.as_ref().map(|v| v.len()), Some(0));
    }
}

#[cfg(test)]
mod providers_consulted_tests {
    use super::merge_providers_consulted;

    #[test]
    fn web_stays_first_extras_unique() {
        let out = merge_providers_consulted(
            "tavily".into(),
            Some("xai".into()),
            vec!["firecrawl".into(), "tavily".into(), "firecrawl".into()],
        );
        assert_eq!(out, vec!["tavily", "xai", "firecrawl"]);
    }

    #[test]
    fn no_social_scrape_only() {
        let out = merge_providers_consulted("blend".into(), None, vec!["firecrawl".into()]);
        assert_eq!(out, vec!["blend", "firecrawl"]);
    }
}

#[cfg(test)]
mod scrape_mapper_tests {
    use super::{scraped_page_from_extract, select_scrape_targets};
    use serpotter_core::SearchItem;

    fn item(title: &str, url: &str) -> SearchItem {
        SearchItem {
            title: title.into(),
            url: url.into(),
            snippet: None,
            content: None,
            score: None,
            published: None,
            author: None,
            provider: None,
            source: None,
        }
    }

    #[test]
    fn select_filters_empty_before_take() {
        let items = vec![
            item("a", ""),
            item("b", "https://b.example"),
            item("c", "https://c.example"),
            item("d", "https://d.example"),
        ];
        let out = select_scrape_targets(&items, 2);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "https://b.example");
        assert_eq!(out[1].0, "https://c.example");
    }

    #[test]
    fn select_take_zero() {
        let items = vec![item("a", "https://a.example")];
        assert!(select_scrape_targets(&items, 0).is_empty());
    }

    #[test]
    fn content_gated_off_keeps_excerpt() {
        let page = scraped_page_from_extract(
            Some("t".into()),
            "https://x".into(),
            "full body text here".into(),
            false,
        );
        assert!(page.content.is_none());
        assert_eq!(page.excerpt.as_deref(), Some("full body text here"));
        assert!(page.error.is_none());
    }

    #[test]
    fn content_gated_on_includes_full() {
        let page = scraped_page_from_extract(None, "https://x".into(), "BODY".into(), true);
        assert_eq!(page.content.as_deref(), Some("BODY"));
        assert_eq!(page.excerpt.as_deref(), Some("BODY"));
    }
}
