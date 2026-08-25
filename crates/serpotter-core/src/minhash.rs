//! MinHash near-duplicate suppression over merged result lists.
//!
//! URL normalization catches exact-duplicate URLs and RRF fuses keyed dupes;
//! what slips through is the same page surfaced under different URLs
//! (syndicated copies, slug variants) or URL-less answers with near-identical
//! text. A 64-dim MinHash signature over character 4-grams estimates Jaccard
//! similarity; greedy keep-first suppression drops any later item that is
//! near-identical to an already-kept (higher-ranked) item.
//!
//! Character grams (not word shingles) keep this language-agnostic. Pure
//! free-fns per crate convention; deterministic fixed-seed hashing so results
//! are reproducible across runs.

use crate::types::SearchItem;

/// Signature dimensionality — 64 gives ±1/8 Jaccard resolution at 0.9,
/// far inside the margin that matters at this threshold.
const SIG_DIM: usize = 64;
/// Character n-gram width.
const GRAM: usize = 4;
/// Drop a later item when estimated Jaccard vs a kept item reaches this.
/// Deliberately conservative: distinct articles on the same topic sit far
/// below 0.9; syndicated copies of the same text sit above it.
const SIMILARITY_THRESHOLD: f64 = 0.9;
/// Cap of fingerprinted text per item (chars) — snippets/answers only need
/// their head to identify a near-copy.
const TEXT_CAP_CHARS: usize = 2_000;

fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Fingerprint basis: title + snippet + content, lowercased, whitespace
/// collapsed, head-capped. `None` for items without enough text to have a
/// usable identity — those are always kept.
fn fingerprint_text(item: &SearchItem) -> Option<String> {
    let mut text = String::from(&item.title);
    if let Some(s) = item.snippet.as_deref() {
        text.push(' ');
        text.push_str(s);
    }
    if let Some(c) = item.content.as_deref() {
        text.push(' ');
        text.push_str(c);
    }
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = collapsed
        .to_lowercase()
        .chars()
        .take(TEXT_CAP_CHARS)
        .collect();
    if chars.len() < GRAM * 4 {
        return None;
    }
    Some(chars.into_iter().collect())
}

/// Hashes of every character 4-gram. A `0xFF` separator byte between scalars
/// keeps ("ab", "c") distinct from ("a", "bc").
fn gram_hashes(text: &[char]) -> Vec<u64> {
    text.windows(GRAM)
        .map(|w| {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for &c in w {
                let mut buf = [0u8; 4];
                for &b in c.encode_utf8(&mut buf).as_bytes() {
                    h ^= b as u64;
                    h = h.wrapping_mul(0x0000_0100_0000_01b3);
                }
                h ^= 0xff;
            }
            h
        })
        .collect()
}

fn signature(grams: &[u64]) -> [u64; SIG_DIM] {
    let mut sig = [u64::MAX; SIG_DIM];
    for (i, slot) in sig.iter_mut().enumerate() {
        for &g in grams {
            let v = splitmix64(g ^ (i as u64));
            if v < *slot {
                *slot = v;
            }
        }
    }
    sig
}

fn similarity(a: &[u64; SIG_DIM], b: &[u64; SIG_DIM]) -> f64 {
    a.iter().zip(b.iter()).filter(|(x, y)| x == y).count() as f64 / SIG_DIM as f64
}

/// Greedy near-duplicate suppression over an already-ranked list: the first
/// occurrence (highest RRF score) is kept untouched — score, provider, and
/// order all preserved — later near-identical items are dropped.
pub fn dedupe_near_duplicates(items: Vec<SearchItem>) -> Vec<SearchItem> {
    let mut kept: Vec<(Option<[u64; SIG_DIM]>, SearchItem)> = Vec::with_capacity(items.len());
    for item in items {
        let sig = fingerprint_text(&item)
            .map(|t| signature(&gram_hashes(&t.chars().collect::<Vec<_>>())));
        let duplicate = match &sig {
            // Identity-less items can never be proven near-duplicates.
            None => false,
            Some(sig) => kept.iter().any(|(kept_sig, _)| {
                kept_sig.is_some_and(|ks| similarity(&ks, sig) >= SIMILARITY_THRESHOLD)
            }),
        };
        if !duplicate {
            kept.push((sig, item));
        }
    }
    kept.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn news_item(title: &str, body: &str, url: &str) -> SearchItem {
        SearchItem {
            title: title.into(),
            url: url.into(),
            snippet: Some(body.into()),
            content: None,
            score: None,
            published: None,
            author: None,
            provider: Some("tavily".into()),
            source: Some("web".into()),
        }
    }

    const ARTICLE_A: &str = "central bank raises policy rate by 25 basis points citing persistent inflation pressure and signals further tightening at the next meeting";
    const ARTICLE_B: &str = "finance ministry unveils a new small business tax credit program aimed at supporting digital transformation projects across the manufacturing sector";

    #[test]
    fn syndicated_near_copy_is_dropped() {
        // Same article text, trivial edits, different domains — URL normalize
        // cannot catch this; MinHash must.
        let items = vec![
            news_item("Rate Hike", ARTICLE_A, "https://news.example/rate-hike"),
            news_item(
                "Rate Hike",
                &(ARTICLE_A.to_string() + " officials said."),
                "https://mirror.example/2026/08/rate-hike-syndicated",
            ),
        ];
        let out = dedupe_near_duplicates(items);
        assert_eq!(out.len(), 1, "near-identical copy must be dropped");
        assert_eq!(out[0].url, "https://news.example/rate-hike");
    }

    #[test]
    fn distinct_articles_are_kept() {
        let items = vec![
            news_item("Rates", ARTICLE_A, "https://a.example/x"),
            news_item("Tax", ARTICLE_B, "https://b.example/y"),
        ];
        let out = dedupe_near_duplicates(items);
        assert_eq!(out.len(), 2, "distinct topics must never merge");
    }

    #[test]
    fn order_scores_and_providers_are_preserved() {
        let mut first = news_item("A", ARTICLE_A, "https://a.example/x");
        first.score = Some(0.75);
        first.provider = Some("exa".into());
        let second = news_item("B", ARTICLE_B, "https://b.example/y");
        let out = dedupe_near_duplicates(vec![first, second]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].score, Some(0.75));
        assert_eq!(out[0].provider.as_deref(), Some("exa"));
    }

    #[test]
    fn identity_less_items_are_never_dropped() {
        let thin = || news_item("", "", "");
        let out = dedupe_near_duplicates(vec![thin(), thin()]);
        assert_eq!(out.len(), 2, "stubs carry no fingerprintable identity");
    }

    #[test]
    fn identical_url_less_answers_merge_to_one() {
        let answer = || news_item("Answer", ARTICLE_A, "");
        let out = dedupe_near_duplicates(vec![answer(), answer()]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn empty_input_is_a_no_op() {
        assert!(dedupe_near_duplicates(Vec::new()).is_empty());
    }
}
