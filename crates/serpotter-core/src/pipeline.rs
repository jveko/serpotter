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

fn result_key(item: &SearchItem) -> String {
    if !item.url.is_empty() {
        return normalize_url(&item.url);
    }
    // URL-less items (some xAI/Exa legs) key on their content alone — never
    // the list rank, or the SAME item returned at different ranks by two legs
    // would fuse as two distinct results and RRF could never merge them.
    format!(
        "__no_url_{}_{}_{}",
        item.title,
        item.snippet.as_deref().unwrap_or(""),
        item.content
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(64)
            .collect::<String>()
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

/// Reciprocal Rank Fusion: score(d) = Σ w · q / (k + rank), k=60.
pub fn reciprocal_rank_fusion(lists: &[RrfList<'_>]) -> Vec<SearchItem> {
    let mut scores: HashMap<String, (f64, SearchItem)> = HashMap::new();

    for list in lists {
        for (rank, item) in list.items.iter().enumerate() {
            let key = result_key(item);
            let contrib = (list.weight * quality(item)) / (RRF_K + rank as f64);
            match scores.get_mut(&key) {
                Some((score, existing)) => {
                    *score += contrib;
                    if richness(item) > richness(existing) {
                        *existing = item.clone();
                    }
                    // FU08: the wire score must be the fused composite, not the
                    // raw provider score of the richest duplicate — set AFTER
                    // the richness swap, or a richer later duplicate (raw or
                    // None score from its own leg) would reset the wire score.
                    existing.score = Some(*score);
                }
                None => {
                    let mut item = item.clone();
                    item.score = Some(contrib);
                    scores.insert(key, (contrib, item));
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
        let a = vec![
            item("a", "https://a.example/"),
            item("b", "https://b.example/"),
        ];
        let b = vec![
            item("a", "https://a.example/?utm_source=x"),
            item("c", "https://c.example/"),
        ];
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

    fn no_url_item(title: &str, snippet: &str) -> SearchItem {
        SearchItem {
            title: title.into(),
            url: String::new(),
            snippet: Some(snippet.into()),
            content: None,
            score: None,
            published: None,
            author: None,
            provider: Some("xai".into()),
            source: Some("x".into()),
        }
    }

    /// D1/FU03: the SAME URL-less item returned by two legs at different ranks
    /// must merge into one result (the rank must not be part of the dedupe key).
    #[test]
    fn no_url_items_merge_across_ranks() {
        let a = vec![
            no_url_item("dup", "a sufficiently long snippet"),
            no_url_item("other", "another sufficiently long snippet"),
        ];
        // The same URL-less item appears at a DIFFERENT rank in the second list
        // — the old key embedded `rank`, so this never merged.
        let b = vec![
            no_url_item("filler", "filler sufficiently long snippet"),
            no_url_item("dup", "a sufficiently long snippet"),
        ];
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
        let dups: Vec<&SearchItem> = out.iter().filter(|i| i.title == "dup").collect();
        assert_eq!(
            dups.len(),
            1,
            "the URL-less duplicate must fuse into one item: {out:?}"
        );
    }

    /// D2/FU08: after fusion every kept item carries the fused RRF score, and a
    /// merged duplicate sums both contributions (rank 0 + rank 0 = two terms).
    #[test]
    fn fused_score_replaces_raw_provider_score() {
        let raw = item("dup", "https://dup.example/");
        let mut rawer = item("dup", "https://dup.example/");
        rawer.score = Some(0.2); // provider-originated raw score must not survive
        let b = vec![rawer];
        let out = reciprocal_rank_fusion(&[
            RrfList {
                items: &[raw],
                weight: 1.0,
            },
            RrfList {
                items: &b,
                weight: 1.0,
            },
        ]);
        assert_eq!(out.len(), 1, "identical URL + title must dedupe");
        let score = out[0].score.expect("fused score must be Some");
        // two identical items at rank 0: k=60, q=1.0 each leg → 2 * (1.0 / 61).
        let expected = 2.0 * 1.0 / (RRF_K + 0.0);
        assert!(
            (score - expected).abs() < 1e-9,
            "expected fused {expected}, got {score}"
        );
    }

    #[test]
    fn fused_score_survives_richer_duplicate_swap() {
        // A richer duplicate arriving in a LATER list replaces the kept item;
        // its raw/None score must not clobber the fused composite (regression
        // for setting score before the richness swap).
        let thin = SearchItem {
            title: "dup".into(),
            url: "https://dup.example/".into(),
            snippet: Some("tiny".into()),
            content: None,
            score: Some(0.99), // raw provider score of the thin first copy
            published: None,
            author: None,
            provider: Some("tavily".into()),
            source: Some("web".into()),
        };
        let rich = SearchItem {
            title: "dup".into(),
            url: "https://dup.example/".into(),
            snippet: Some("this is a sufficiently long snippet".into()),
            content: Some("and a sufficiently long content body here too".into()),
            score: None, // richer copy carries NO raw score (firecrawl/xai legs)
            published: None,
            author: None,
            provider: Some("firecrawl".into()),
            source: Some("web".into()),
        };
        let out = reciprocal_rank_fusion(&[
            RrfList {
                items: &[thin],
                weight: 1.0,
            },
            RrfList {
                items: &[rich],
                weight: 1.0,
            },
        ]);
        assert_eq!(out.len(), 1, "same URL dedupes");
        // The richer copy won the swap but must carry the FUSED score, not None.
        assert_eq!(out[0].provider.as_deref(), Some("firecrawl"));
        // thin copy quality 0.25 + rich copy quality 1.0, both at rank 0.
        let expected = (QUALITY_THIN + QUALITY_FULL) / (RRF_K + 0.0);
        let score = out[0].score.expect("fused score must survive the swap");
        assert!(
            (score - expected).abs() < 1e-9,
            "expected fused {expected}, got {score}"
        );
    }

    #[test]
    fn disjoint_no_url_items_keep_own_scores() {
        let a = vec![no_url_item("a", "a sufficiently long snippet")];
        let b = vec![no_url_item("b", "b sufficiently long snippet")];
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
        assert_eq!(out.len(), 2, "distinct URL-less items stay separate");
        for item in &out {
            assert!(item.score.is_some(), "every fused item carries a score");
        }
    }
}
