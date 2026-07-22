//! Result pipeline: URL dedupe + reciprocal rank fusion (k=60).

use crate::types::SearchItem;
use crate::url_normalize::normalize_url;
use std::collections::HashMap;

const RRF_K: f64 = 60.0;
const SNIPPET_MIN: usize = 20;
const CONTENT_MIN: usize = 40;
const QUALITY_FULL: f64 = 1.0;
const QUALITY_THIN: f64 = 0.25;

pub struct RrfList<'a> {
    pub items: &'a [SearchItem],
    pub weight: f64,
}

fn result_key(item: &SearchItem, rank: usize) -> String {
    if !item.url.is_empty() {
        return normalize_url(&item.url);
    }
    format!(
        "__no_url_{}_{}_{}_{}",
        rank,
        item.title,
        item.snippet.as_deref().unwrap_or(""),
        item.content.as_deref().unwrap_or("").chars().take(64).collect::<String>()
    )
}

fn quality(item: &SearchItem) -> f64 {
    let snippet_len = item.snippet.as_ref().map(|s| s.trim().len()).unwrap_or(0);
    let content_len = item.content.as_ref().map(|s| s.trim().len()).unwrap_or(0);
    if snippet_len >= SNIPPET_MIN || content_len >= CONTENT_MIN {
        QUALITY_FULL
    } else {
        QUALITY_THIN
    }
}

fn richness(item: &SearchItem) -> usize {
    item.snippet.as_ref().map(|s| s.trim().len()).unwrap_or(0)
        + item.content.as_ref().map(|s| s.trim().len()).unwrap_or(0)
}

/// Deduplicate by normalized URL; items without URL are kept.
pub fn dedupe_by_url(items: &[SearchItem]) -> Vec<SearchItem> {
    let mut seen = std::collections::HashSet::new();
    items
        .iter()
        .filter(|item| {
            if item.url.is_empty() {
                return true;
            }
            let key = normalize_url(&item.url);
            seen.insert(key)
        })
        .cloned()
        .collect()
}

/// Reciprocal Rank Fusion: score(d) = Σ w · q / (k + rank), k=60.
pub fn reciprocal_rank_fusion(lists: &[RrfList<'_>]) -> Vec<SearchItem> {
    let mut scores: HashMap<String, (f64, SearchItem)> = HashMap::new();

    for list in lists {
        for (rank, item) in list.items.iter().enumerate() {
            let key = result_key(item, rank);
            let contrib = (list.weight * quality(item)) / (RRF_K + rank as f64);
            match scores.get_mut(&key) {
                Some((score, existing)) => {
                    *score += contrib;
                    if richness(item) > richness(existing) {
                        *existing = item.clone();
                    }
                }
                None => {
                    scores.insert(key, (contrib, item.clone()));
                }
            }
        }
    }

    let mut merged: Vec<(f64, SearchItem)> = scores.into_values().collect();
    merged.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    merged.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(title: &str, url: &str) -> SearchItem {
        SearchItem {
            title: title.into(),
            url: url.into(),
            snippet: Some("this is a sufficiently long snippet for quality".into()),
            content: None,
            score: None,
            published: None,
            author: None,
            provider: Some("tavily".into()),
            source: Some("web".into()),
        }
    }

    #[test]
    fn rrf_prefers_top_of_both_lists() {
        let a = vec![item("a", "https://a.example/"), item("b", "https://b.example/")];
        let b = vec![item("a", "https://a.example/?utm_source=x"), item("c", "https://c.example/")];
        let out = reciprocal_rank_fusion(&[
            RrfList {
                items: &a,
                weight: 1.0,
            },
            RrfList {
                items: &b,
                weight: 1.0,
            },
        ]);
        assert_eq!(out[0].title, "a");
        assert!(out.len() >= 2);
    }

    #[test]
    fn dedupe_collapses_tracking_urls() {
        let items = vec![
            item("a", "https://example.com/p?utm_source=x"),
            item("a2", "https://www.example.com/p"),
        ];
        let out = dedupe_by_url(&items);
        assert_eq!(out.len(), 1);
    }
}
