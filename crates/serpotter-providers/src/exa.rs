use crate::{ExtractResult, ProviderError, ProviderResult, ProviderSearchParams};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

/// Thin Exa adapter — HTTP client is supplied per call (registry cache).
#[derive(Clone)]
pub struct ExaClient {
    base_url: String,
}

/// Caps the per-page text Exa `/contents` returns (characters) — bounded so a
/// single rogue page cannot balloon the extract body (personal-use budget).
const CONTENTS_MAX_CHARACTERS: u32 = 10_000;

/// Caps the per-page highlight text Exa `/contents` returns (characters,
/// `highlights.maxCharacters` — the documented size key since the legacy
/// numSentences/highlightsPerUrl knobs were deprecated upstream).
const HIGHLIGHTS_MAX_CHARACTERS: u32 = 4_000;

impl ExaClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn search(
        &self,
        http: &Client,
        p: ProviderSearchParams<'_>,
    ) -> Result<ProviderResult, ProviderError> {
        let url = format!("{}/search", self.base_url);
        let mut contents = serde_json::json!({ "highlights": true });
        if p.include_content {
            contents["text"] = serde_json::json!(true);
        }
        let mut body = serde_json::json!({
            "query": p.query,
            "numResults": p.max_results,
            "contents": contents,
        });
        if let Some(d) = p.include_domains {
            if !d.is_empty() {
                body["includeDomains"] = serde_json::json!(d);
            }
        }
        if let Some(d) = p.exclude_domains {
            if !d.is_empty() {
                body["excludeDomains"] = serde_json::json!(d);
            }
        }
        apply_exa_date_filters(&mut body, p.from_date, p.to_date, p.time_range);

        let res = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", p.api_key))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }

        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
            /// Exact per-call cost in USD reported by Exa (B2/B22 cost capture).
            #[serde(rename = "costDollars")]
            cost_dollars: Option<f64>,
        }
        #[derive(Deserialize)]
        struct Row {
            title: Option<String>,
            url: Option<String>,
            text: Option<String>,
            summary: Option<String>,
            highlights: Option<Vec<String>>,
            score: Option<f64>,
            #[serde(rename = "publishedDate")]
            published_date: Option<String>,
        }

        let up: Up = res.json().await?;
        let items = up
            .results
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let snippet = r
                    .highlights
                    .map(|h| h.join(" ... "))
                    .filter(|s| !s.is_empty())
                    .or(r.summary)
                    .or(r.text.clone());
                SearchItem {
                    title: r.title.unwrap_or_default(),
                    url: r.url.unwrap_or_default(),
                    snippet,
                    content: if p.include_content { r.text } else { None },
                    score: r.score,
                    published: r.published_date,
                    author: None,
                    provider: Some("exa".into()),
                    source: Some("web".into()),
                }
            })
            .collect();

        Ok(ProviderResult {
            provider: "exa".into(),
            query: p.query.to_string(),
            items,
            answer: None,
            // Exa reports an exact per-call dollar figure — carry it verbatim.
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost: up.cost_dollars,
        })
    }

    /// Fetch page content via Exa `POST /contents` (ids accept bare URLs).
    ///
    /// Best-effort per page: a missing/errored result row maps to
    /// [`ProviderError::Unextractable`] — never a panic and never an HTTP
    /// health signal (product must release holds and continue the chain).
    pub async fn extract(
        &self,
        http: &Client,
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        let endpoint = format!("{}/contents", self.base_url);
        let body = serde_json::json!({
            "ids": [url],
            "text": { "maxCharacters": CONTENTS_MAX_CHARACTERS },
        });
        let res = http
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
            /// Exact per-call cost in USD reported by Exa (B2/B22 cost capture).
            #[serde(rename = "costDollars")]
            cost_dollars: Option<f64>,
        }
        #[derive(Deserialize)]
        struct Row {
            title: Option<String>,
            url: Option<String>,
            text: Option<String>,
            /// Per-row failure reported by the vendor (best-effort surface).
            error: Option<String>,
        }
        let up: Up = res.json().await?;
        match up.results.unwrap_or_default().into_iter().next() {
            Some(row) => {
                if row.text.is_none() {
                    let msg = row
                        .error
                        .unwrap_or_else(|| "contents returned no text".into());
                    return Err(ProviderError::Unextractable {
                        provider: "exa".into(),
                        message: msg,
                    });
                }
                Ok(ExtractResult {
                    url: row.url.unwrap_or_else(|| url.to_string()),
                    title: row.title,
                    content: row.text.unwrap_or_default(),
                    provider: "exa".into(),
                    cost: up.cost_dollars,
                })
            }
            None => Err(ProviderError::Unextractable {
                provider: "exa".into(),
                message: "contents returned no results for the requested url".into(),
            }),
        }
    }

    /// Generate a cited answer via Exa `POST /answer` (B20).
    ///
    /// Sync endpoint: the wire returns `answer` (string, or an object when
    /// structured), `citations` and `costDollars.total` (exact per-call cost —
    /// carried verbatim). Body mirrors the official exa-js SDK: `query`,
    /// `stream: false`, `text: false`, `model: "exa"`.
    ///
    /// HONESTY (verified against the official exa-js SDK + docs 2026-08):
    /// `POST /answer` documents NO max-results parameter and NO deep-mode
    /// parameter. Both are refused locally via [`ProviderError::Unsupported`]
    /// — never silently dropped — with pointers to the endpoints that do
    /// express them (`/search` `numResults`, deep modes on
    /// [`Self::search_deep`]).
    pub async fn answer(
        &self,
        http: &Client,
        api_key: &str,
        query: &str,
        max_results: Option<u32>,
        deep: bool,
    ) -> Result<ExaAnswer, ProviderError> {
        if let Some(n) = max_results {
            return Err(ProviderError::Unsupported {
                provider: "exa".into(),
                action: "answer",
                detail: format!(
                    "max_results={n} is not expressible on POST /answer (no max-results parameter documented); use search_deep (numResults) or /search for result-count control"
                ),
            });
        }
        if deep {
            return Err(ProviderError::Unsupported {
                provider: "exa".into(),
                action: "answer",
                detail: "deep mode is not expressible on POST /answer (no deep parameter documented); use search_deep with mode deep-lite|deep|deep-reasoning for deep research".into(),
            });
        }
        let url = format!("{}/answer", self.base_url);
        let body = serde_json::json!({
            "query": query,
            "stream": false,
            "text": false,
            "model": "exa",
        });
        let res = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            answer: Option<serde_json::Value>,
            citations: Option<Vec<Cit>>,
            #[serde(rename = "costDollars")]
            cost_dollars: Option<Cost>,
        }
        #[derive(Deserialize)]
        struct Cit {
            title: Option<String>,
            url: Option<String>,
        }
        #[derive(Deserialize)]
        struct Cost {
            total: Option<f64>,
        }
        let up: Up = res.json().await?;
        let answer = match up.answer {
            Some(serde_json::Value::String(s)) => s,
            Some(v) => v.to_string(), // structured object — compact JSON passthrough
            None => String::new(),
        };
        let citations = up
            .citations
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| {
                let url = c.url?;
                Some(ExaCitation {
                    title: c.title.unwrap_or_default(),
                    url,
                })
            })
            .collect();
        Ok(ExaAnswer {
            answer,
            citations,
            cost: up.cost_dollars.and_then(|c| c.total),
        })
    }

    /// Find pages similar to a URL via Exa `POST /findSimilar` (B24).
    ///
    /// Body sends only `url` + optional `numResults` — deliberately NO
    /// contents block, so the (costly, $1/1k pages) full-text payload is not
    /// fetched; the returned [`ExaSimilarItem`]s are title+url, matching the
    /// Wave 3B wire shape `{items: [{title, url}]}`. Rows without a URL are
    /// dropped (same filter policy as xAI citations).
    pub async fn find_similar(
        &self,
        http: &Client,
        api_key: &str,
        url: &str,
        max_results: Option<u32>,
    ) -> Result<Vec<ExaSimilarItem>, ProviderError> {
        let endpoint = format!("{}/findSimilar", self.base_url);
        let mut body = serde_json::json!({ "url": url });
        if let Some(n) = max_results {
            body["numResults"] = serde_json::json!(n);
        }
        let res = http
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
        }
        #[derive(Deserialize)]
        struct Row {
            title: Option<String>,
            url: Option<String>,
        }
        let up: Up = res.json().await?;
        Ok(up
            .results
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let url = r.url?;
                Some(ExaSimilarItem {
                    title: r.title.unwrap_or_default(),
                    url,
                })
            })
            .collect())
    }

    /// Deep (embeddings-based) search via Exa `POST /search` (B20/B29).
    ///
    /// `mode` must be `deep-lite`, `deep` or `deep-reasoning` (the `type`
    /// wire enum — validated locally before any network call). `output_schema`
    /// is the B28 structured-output passthrough: Exa documents `outputSchema`
    /// on `/search` for EVERY search type; when set, the synthesized answer
    /// (string or structured object) comes back in the wire `output.content`,
    /// surfaced here as [`ExaDeepSearch::output`].
    ///
    /// B29 note: deep modes are embeddings-based SERVER-SIDE reranking, so
    /// this method doubles as the semantic-rerank leg — no local embedding
    /// work is needed (the design's J2-7 decision). Results carry bounded
    /// page text (`contents.text.maxCharacters` = [`CONTENTS_MAX_CHARACTERS`])
    /// so product can use them as evidence without a second /contents call.
    pub async fn search_deep(
        &self,
        http: &Client,
        api_key: &str,
        query: &str,
        mode: &str,
        max_results: Option<u32>,
        output_schema: Option<&serde_json::Value>,
    ) -> Result<ExaDeepSearch, ProviderError> {
        match mode {
            "deep-lite" | "deep" | "deep-reasoning" => {}
            other => {
                return Err(ProviderError::Unsupported {
                    provider: "exa".into(),
                    action: "search",
                    detail: format!(
                        "type={other} is not a deep mode (documented deep types: deep-lite, deep, deep-reasoning)"
                    ),
                });
            }
        }
        let url = format!("{}/search", self.base_url);
        let mut body = serde_json::json!({
            "query": query,
            "type": mode,
            "contents": { "text": { "maxCharacters": CONTENTS_MAX_CHARACTERS } },
        });
        if let Some(n) = max_results {
            body["numResults"] = serde_json::json!(n);
        }
        if let Some(s) = output_schema {
            body["outputSchema"] = s.clone();
        }
        let res = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
            output: Option<Out>,
            #[serde(rename = "costDollars")]
            cost_dollars: Option<Cost>,
        }
        #[derive(Deserialize)]
        struct Row {
            title: Option<String>,
            url: Option<String>,
            text: Option<String>,
            score: Option<f64>,
        }
        #[derive(Deserialize)]
        struct Out {
            content: Option<serde_json::Value>,
        }
        #[derive(Deserialize)]
        struct Cost {
            total: Option<f64>,
        }
        let up: Up = res.json().await?;
        let items = up
            .results
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let url = r.url?;
                Some(ExaDeepItem {
                    title: r.title.unwrap_or_default(),
                    url,
                    content: r.text.filter(|t| !t.trim().is_empty()),
                    score: r.score,
                })
            })
            .collect();
        Ok(ExaDeepSearch {
            items,
            output: up.output.and_then(|o| o.content),
            cost: up.cost_dollars.and_then(|c| c.total),
        })
    }

    /// Extract multiple URLs in one call via Exa `POST /contents` (B26).
    ///
    /// Same wire family as [`Self::extract`] (`ids` + bounded `text`), one
    /// request for the whole list. Batch semantics: rows that returned text
    /// become [`ExaExtractedPage`]s; rows without text (vendor failure or an
    /// unreachable page) are simply absent — one bad URL never fails the
    /// whole batch.
    pub async fn extract_batch(
        &self,
        http: &Client,
        api_key: &str,
        urls: &[String],
    ) -> Result<Vec<ExaExtractedPage>, ProviderError> {
        let endpoint = format!("{}/contents", self.base_url);
        let body = serde_json::json!({
            "ids": urls,
            "text": { "maxCharacters": CONTENTS_MAX_CHARACTERS },
        });
        let res = http
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
        }
        #[derive(Deserialize)]
        struct Row {
            url: Option<String>,
            text: Option<String>,
        }
        let up: Up = res.json().await?;
        Ok(up
            .results
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let content = r.text?;
                if content.trim().is_empty() {
                    return None;
                }
                Some(ExaExtractedPage {
                    url: r.url.unwrap_or_default(),
                    content,
                })
            })
            .collect())
    }

    /// Extract a page's highlight sentences via Exa `POST /contents` (B27).
    ///
    /// Sends `ids: [url]` + `highlights` (bounded [`HIGHLIGHTS_MAX_CHARACTERS`],
    /// the documented size key — the legacy numSentences/highlightsPerUrl
    /// knobs are deprecated upstream). The first result row's highlights are
    /// joined with newlines; when the vendor returns none, the page text is
    /// returned as an honest fallback; a row with neither is URL-class
    /// [`ProviderError::Unextractable`] (never a fake HTTP health signal).
    pub async fn extract_highlights(
        &self,
        http: &Client,
        api_key: &str,
        url: &str,
    ) -> Result<String, ProviderError> {
        let endpoint = format!("{}/contents", self.base_url);
        let body = serde_json::json!({
            "ids": [url],
            "highlights": { "maxCharacters": HIGHLIGHTS_MAX_CHARACTERS },
        });
        let res = http
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "exa".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
        }
        #[derive(Deserialize)]
        struct Row {
            highlights: Option<Vec<String>>,
            text: Option<String>,
        }
        let up: Up = res.json().await?;
        match up.results.unwrap_or_default().into_iter().next() {
            Some(row) => {
                let joined = row
                    .highlights
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|h| !h.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !joined.is_empty() {
                    return Ok(joined);
                }
                if let Some(t) = row.text.filter(|t| !t.trim().is_empty()) {
                    return Ok(t);
                }
                Err(ProviderError::Unextractable {
                    provider: "exa".into(),
                    message: "contents returned no highlights or text for the requested url".into(),
                })
            }
            None => Err(ProviderError::Unextractable {
                provider: "exa".into(),
                message: "contents returned no results for the requested url".into(),
            }),
        }
    }
}

/// Cited answer from Exa `POST /answer` (B20). `cost` is the exact per-call
/// dollar figure from `costDollars.total` (never an estimate).
#[derive(Debug, Clone)]
pub struct ExaAnswer {
    pub answer: String,
    pub citations: Vec<ExaCitation>,
    pub cost: Option<f64>,
}

/// One cited source of an Exa answer.
#[derive(Debug, Clone)]
pub struct ExaCitation {
    pub title: String,
    pub url: String,
}

/// Result of a deep search via Exa `POST /search` (B20/B29).
///
/// `items` are the ranked (embeddings-based) results with bounded page text;
/// `output` carries the B28 synthesized answer when `outputSchema` was sent
/// (Exa returns it in the wire `output.content` — a string, or a structured
/// object) and is `None` otherwise; `cost` is `costDollars.total`.
#[derive(Debug, Clone)]
pub struct ExaDeepSearch {
    pub items: Vec<ExaDeepItem>,
    pub output: Option<serde_json::Value>,
    pub cost: Option<f64>,
}

/// One deep-search result item.
#[derive(Debug, Clone)]
pub struct ExaDeepItem {
    pub title: String,
    pub url: String,
    pub content: Option<String>,
    pub score: Option<f64>,
}

/// One similar-page hit from Exa `POST /findSimilar` (B24) — title+url per
/// the Wave 3B wire shape `{items: [{title, url}]}`.
#[derive(Debug, Clone)]
pub struct ExaSimilarItem {
    pub title: String,
    pub url: String,
}

/// One extracted page of an Exa `/contents` batch call (B26).
#[derive(Debug, Clone)]
pub struct ExaExtractedPage {
    pub url: String,
    pub content: String,
}

/// Set Exa startPublishedDate / endPublishedDate.
/// Absolute dates win; when only `time_range` is set, map day/week/month/year to a
/// relative start date (UTC YYYY-MM-DD) so Exa does not silently drop the filter.
pub(crate) fn apply_exa_date_filters(
    body: &mut serde_json::Value,
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
) {
    let has_abs = from_date.is_some() || to_date.is_some();
    if let Some(d) = from_date {
        body["startPublishedDate"] = serde_json::json!(d);
    }
    if let Some(d) = to_date {
        body["endPublishedDate"] = serde_json::json!(d);
    }
    if !has_abs {
        if let Some(start) = exa_start_from_time_range(time_range) {
            body["startPublishedDate"] = serde_json::json!(start);
        }
    }
}

/// Map relative time_range → start ISO date (UTC), approx month=30d year=365d.
fn exa_start_from_time_range(time_range: Option<&str>) -> Option<String> {
    let days = match time_range.map(str::trim)? {
        "day" => 1u64,
        "week" => 7,
        "month" => 30,
        "year" => 365,
        _ => return None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let secs = now.as_secs().saturating_sub(days.saturating_mul(86_400));
    Some(unix_secs_to_ymd(secs))
}

/// Civil YYYY-MM-DD from Unix seconds (UTC). Howard Hinnant civil_from_days.
fn unix_secs_to_ymd(secs: u64) -> String {
    let z = (secs / 86_400) as i64;
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{:02}-{:02}", m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_dates_set_when_present() {
        let mut body = serde_json::json!({});
        apply_exa_date_filters(
            &mut body,
            Some("2026-02-01"),
            Some("2026-02-28"),
            Some("week"),
        );
        assert_eq!(body["startPublishedDate"], "2026-02-01");
        assert_eq!(body["endPublishedDate"], "2026-02-28");
    }

    #[test]
    fn published_dates_none_leaves_body() {
        let mut body = serde_json::json!({ "query": "q" });
        apply_exa_date_filters(&mut body, None, None, None);
        assert!(body.get("startPublishedDate").is_none());
        assert!(body.get("endPublishedDate").is_none());
        assert_eq!(body["query"], "q");
    }

    #[test]
    fn time_range_week_sets_start_when_no_abs() {
        let mut body = serde_json::json!({});
        apply_exa_date_filters(&mut body, None, None, Some("week"));
        let start = body["startPublishedDate"].as_str().expect("start");
        assert_eq!(start.len(), 10, "{start}");
        assert!(body.get("endPublishedDate").is_none());
    }

    #[test]
    fn abs_dates_skip_time_range() {
        let mut body = serde_json::json!({});
        apply_exa_date_filters(&mut body, Some("2026-01-01"), None, Some("year"));
        assert_eq!(body["startPublishedDate"], "2026-01-01");
        assert!(body.get("endPublishedDate").is_none());
    }

    #[test]
    fn unix_epoch_ymd() {
        assert_eq!(unix_secs_to_ymd(0), "1970-01-01");
        // 2026-07-26 00:00:00 UTC
        assert_eq!(unix_secs_to_ymd(1_785_052_800), "2026-07-26");
    }

    // --- F47: request-side wire format (path, headers, body field names) -----

    /// Request captured by the loopback recording server.
    struct RecordedRequest {
        request_line: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl RecordedRequest {
        fn path(&self) -> &str {
            self.request_line.split_whitespace().nth(1).unwrap_or("")
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }

        fn body_json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).expect("request body is JSON")
        }
    }

    /// Serve one canned JSON response and capture the request that arrived
    /// (std::thread TcpListener pattern, extended to record wire bytes).
    fn spawn_recording_server(
        response: serde_json::Value,
    ) -> (String, std::sync::mpsc::Receiver<RecordedRequest>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let body = serde_json::to_string(&response).expect("serialize canned response");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain until the declared Content-Length is satisfied (a single
                // read can return a partial request on loopback).
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            let text = String::from_utf8_lossy(&buf);
                            if let Some((head, recv_body)) = text.split_once("\r\n\r\n") {
                                let declared = head
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        (k.trim().eq_ignore_ascii_case("content-length"))
                                            .then(|| v.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                                if recv_body.len() >= declared {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let raw = String::from_utf8_lossy(&buf).to_string();
                let (head, body_part) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("").to_string();
                let headers = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let _ = tx.send(RecordedRequest {
                    request_line,
                    headers,
                    body: body_part.to_string(),
                });
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}"), rx)
    }

    /// Exa search authenticates via Bearer header and uses the camelCase body
    /// keys (numResults/contents/includeDomains/startPublishedDate).
    #[tokio::test]
    async fn search_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [{
                "title": "T", "url": "https://t.example", "text": "body",
                "highlights": ["h1", "h2"], "score": 0.8, "publishedDate": "2026-01-01"
            }],
            "costDollars": 0.003
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let exclude = vec!["spam.example".to_string()];
        let p = ProviderSearchParams {
            query: "rust wire",
            max_results: 4,
            api_key: "exa-secret-key",
            include_content: true,
            include_answer: false,
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: None,
            sources: None,
            include_domains: None,
            exclude_domains: Some(&exclude),
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: Some("2026-03-01"),
            to_date: Some("2026-03-31"),
            time_range: Some("month"),
            country: None,
            exact_match: None,
        };
        let out = client.search(&http, p).await.expect("search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/search", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-secret-key",
            "exa auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["query"], "rust wire");
        assert_eq!(b["numResults"], 4);
        assert_eq!(b["contents"]["highlights"], true);
        assert_eq!(
            b["contents"]["text"], true,
            "include_content → contents.text"
        );
        assert_eq!(b["excludeDomains"], serde_json::json!(["spam.example"]));
        assert_eq!(b["startPublishedDate"], "2026-03-01");
        assert_eq!(b["endPublishedDate"], "2026-03-31");
        // Response parses back: highlights joined as the snippet, text carried.
        assert_eq!(out.items.len(), 1);
        let item = &out.items[0];
        assert_eq!(item.title, "T");
        assert_eq!(item.snippet.as_deref(), Some("h1 ... h2"));
        assert_eq!(item.content.as_deref(), Some("body"));
        assert_eq!(item.published.as_deref(), Some("2026-01-01"));
        let score = item.score.expect("score from wire");
        assert!((score - 0.8).abs() < 1e-9, "score parsed: {score}");
        assert_eq!(item.provider.as_deref(), Some("exa"));
        // costDollars carried verbatim (exact, not an estimate)
        let cost = out.cost.expect("costDollars parsed");
        assert!((cost - 0.003).abs() < 1e-9, "cost parsed: {cost}");
        assert!(out.input_tokens.is_none() && out.output_tokens.is_none());
    }

    /// Exa extract (B10) hits POST /contents with Bearer auth and the
    /// ids/text body; the first result row maps to the shared page shape.
    #[tokio::test]
    async fn extract_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [{
                "title": "Page", "url": "https://example.com/page",
                "text": "# body"
            }],
            "costDollars": 0.001
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .extract(&http, "https://example.com/page", "exa-extract-key")
            .await
            .expect("extract against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/contents", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-extract-key",
            "exa extract auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["ids"], serde_json::json!(["https://example.com/page"]));
        assert_eq!(
            b["text"]["maxCharacters"], 10000,
            "text.maxCharacters const in body: {b}"
        );
        assert_eq!(out.content, "# body");
        assert_eq!(out.title.as_deref(), Some("Page"));
        assert_eq!(out.url, "https://example.com/page");
        assert_eq!(out.provider, "exa");
        let cost = out.cost.expect("extract costDollars parsed");
        assert!((cost - 0.001).abs() < 1e-9, "extract cost parsed: {cost}");
    }

    /// A response without `costDollars` (older/aggregate shapes) leaves
    /// `cost` None — never a fabricated estimate.
    #[tokio::test]
    async fn search_without_cost_dollars_leaves_cost_none() {
        let (base, _rx) = spawn_recording_server(serde_json::json!({
            "results": [{ "title": "T", "url": "https://t.example" }]
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let p = ProviderSearchParams {
            query: "q",
            max_results: 1,
            api_key: "exa-key",
            include_content: false,
            include_answer: false,
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: None,
            sources: None,
            include_domains: None,
            exclude_domains: None,
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: None,
            to_date: None,
            time_range: None,
            country: None,
            exact_match: None,
        };
        let out = client.search(&http, p).await.expect("search against mock");
        assert!(
            out.cost.is_none(),
            "no costDollars -> cost None: {:?}",
            out.cost
        );
        assert_eq!(out.items.len(), 1, "results still parse");
    }

    /// Missing row → clean Unextractable (not a panic, not an HTTP health hit).
    #[tokio::test]
    async fn extract_empty_results_is_unextractable() {
        let (base, _rx) = spawn_recording_server(serde_json::json!({ "results": [] }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let err = client
            .extract(&http, "https://example.com/page", "exa-key")
            .await
            .expect_err("empty results");
        match err {
            ProviderError::Unextractable { provider, message } => {
                assert_eq!(provider, "exa");
                assert!(message.contains("no results"), "{message}");
            }
            other => panic!("expected Unextractable, got {other:?}"),
        }
    }

    // ---- B20: /answer + deep search ----

    /// Exa answer posts /answer with Bearer + the exa-js documented body
    /// (query/stream:false/text:false/model:exa) and parses answer, citations
    /// and the exact costDollars.total.
    #[tokio::test]
    async fn answer_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "answer": "SpaceX is valued at $350 billion.",
            "citations": [
                { "title": "Report", "url": "https://report.example" },
                { "title": "News", "url": "https://news.example" }
            ],
            "requestId": "req-1",
            "costDollars": { "total": 0.005 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .answer(
                &http,
                "exa-answer-key",
                "What is the latest valuation of SpaceX?",
                None,
                false,
            )
            .await
            .expect("answer against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/answer", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-answer-key",
            "answer auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["query"], "What is the latest valuation of SpaceX?");
        assert_eq!(b["stream"], false);
        assert_eq!(b["text"], false);
        assert_eq!(b["model"], "exa");
        assert_eq!(out.answer, "SpaceX is valued at $350 billion.");
        assert_eq!(out.citations.len(), 2);
        assert_eq!(out.citations[0].title, "Report");
        assert_eq!(out.citations[0].url, "https://report.example");
        let cost = out.cost.expect("costDollars.total parsed");
        assert!((cost - 0.005).abs() < 1e-9, "cost parsed: {cost}");
    }

    /// A structured (object) answer is serialized compactly, never dropped.
    #[tokio::test]
    async fn answer_object_answer_serialized() {
        let (base, _rx) = spawn_recording_server(serde_json::json!({
            "answer": { "valuation": "$350B", "currency": "USD" },
            "citations": [],
            "costDollars": { "total": 0.005 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .answer(&http, "exa-answer-key", "q", None, false)
            .await
            .expect("answer against mock");
        let v: serde_json::Value =
            serde_json::from_str(&out.answer).expect("object answer is JSON");
        assert_eq!(v["valuation"], "$350B");
    }

    /// max_results and deep are NOT expressible on the current /answer wire —
    /// refused locally before any network call (never silently dropped).
    #[tokio::test]
    async fn answer_max_results_and_deep_refused_before_network() {
        let client = ExaClient::new("http://127.0.0.1:9");
        let http = crate::http::build_direct();
        let err = client
            .answer(&http, "exa-answer-key", "q", Some(5), false)
            .await
            .expect_err("max_results must be refused");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, "exa");
                assert_eq!(action, "answer");
                assert!(detail.contains("max_results"), "{detail}");
                assert!(detail.contains("search_deep"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
        let err = client
            .answer(&http, "exa-answer-key", "q", None, true)
            .await
            .expect_err("deep must be refused");
        match err {
            ProviderError::Unsupported { detail, .. } => {
                assert!(detail.contains("deep"), "{detail}");
                assert!(detail.contains("search_deep"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    /// search_deep posts /search with type=deep + bounded contents and parses
    /// items (title/url/text). Without outputSchema the synthesized output is
    /// absent and cost is the exact costDollars.total.
    #[tokio::test]
    async fn search_deep_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [
                { "title": "Deep A", "url": "https://a.example", "text": "# body a" },
                { "title": "Deep B", "url": "https://b.example" }
            ],
            "costDollars": { "total": 0.012 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .search_deep(
                &http,
                "exa-key",
                "compare the latest AI models",
                "deep",
                Some(7),
                None,
            )
            .await
            .expect("deep search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/search", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-key",
            "deep search auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["query"], "compare the latest AI models");
        assert_eq!(b["type"], "deep");
        assert_eq!(b["numResults"], 7);
        assert_eq!(
            b["contents"]["text"]["maxCharacters"], 10000,
            "bounded text contents: {b}"
        );
        assert!(
            b.get("outputSchema").is_none(),
            "no outputSchema when None: {b}"
        );
        assert_eq!(out.items.len(), 2);
        assert_eq!(out.items[0].title, "Deep A");
        assert_eq!(out.items[0].content.as_deref(), Some("# body a"));
        assert!(out.items[1].content.is_none(), "row without text -> None");
        assert!(out.output.is_none(), "no outputSchema -> output None");
        let cost = out.cost.expect("deep costDollars.total parsed");
        assert!((cost - 0.012).abs() < 1e-9, "cost parsed: {cost}");
    }

    /// output_schema (B28) passes through to the wire outputSchema key and
    /// the synthesized output comes back in ExaDeepSearch.output.
    #[tokio::test]
    async fn search_deep_output_schema_passthrough() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [],
            "output": { "content": { "models": ["grok-4.5"] } },
            "costDollars": { "total": 0.02 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let schema = serde_json::json!({
            "type": "object",
            "required": ["models"],
            "properties": { "models": { "type": "array" } }
        });
        let out = client
            .search_deep(
                &http,
                "exa-key",
                "compare the latest frontier AI model releases",
                "deep",
                None,
                Some(&schema),
            )
            .await
            .expect("deep search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(
            rec.body_json()["outputSchema"],
            schema,
            "outputSchema key on the wire"
        );
        let out_v = out.output.expect("synthesized output parsed");
        assert_eq!(out_v["models"], serde_json::json!(["grok-4.5"]));
    }

    /// Only the documented deep types ride the wire; anything else is refused
    /// locally before any network call.
    #[tokio::test]
    async fn search_deep_invalid_mode_refused_before_network() {
        let client = ExaClient::new("http://127.0.0.1:9");
        let http = crate::http::build_direct();
        let err = client
            .search_deep(&http, "exa-key", "q", "auto", None, None)
            .await
            .expect_err("non-deep mode must be refused");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, "exa");
                assert_eq!(action, "search");
                assert!(detail.contains("deep-lite"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // ---- B24: /findSimilar ----

    /// find_similar posts /findSimilar with url + optional numResults (no
    /// contents block — the costly text payload is not fetched) and parses
    /// title+url items.
    #[tokio::test]
    async fn find_similar_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [
                { "title": "Similar A", "url": "https://a.example" },
                { "title": "Similar B", "url": "https://b.example" },
                { "title": "No URL", "url": null }
            ],
            "costDollars": { "total": 0.004 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let items = client
            .find_similar(&http, "exa-sim-key", "https://source.example/post", Some(5))
            .await
            .expect("findSimilar against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/findSimilar", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-sim-key",
            "findSimilar auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["url"], "https://source.example/post");
        assert_eq!(b["numResults"], 5);
        assert!(
            b.get("contents").is_none(),
            "no contents block — text payload not fetched: {b}"
        );
        assert_eq!(items.len(), 2, "row without url is dropped");
        assert_eq!(items[0].title, "Similar A");
        assert_eq!(items[0].url, "https://a.example");
    }

    // ---- B26/B27: /contents batch + highlights ----

    /// extract_batch posts ids[] + bounded text in one call and maps every
    /// text-carrying row to a page (text-less rows are skipped, batch wins).
    #[tokio::test]
    async fn extract_batch_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [
                { "url": "https://a.example", "text": "# page a" },
                { "url": "https://b.example", "error": "blocked" }
            ],
            "costDollars": { "total": 0.002 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let urls = vec![
            "https://a.example".to_string(),
            "https://b.example".to_string(),
        ];
        let pages = client
            .extract_batch(&http, "exa-batch-key", &urls)
            .await
            .expect("batch extract against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/contents", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-batch-key",
            "batch auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(
            b["ids"],
            serde_json::json!(["https://a.example", "https://b.example"])
        );
        assert_eq!(b["text"]["maxCharacters"], 10000, "bounded text: {b}");
        assert_eq!(pages.len(), 1, "error row is skipped, batch survives");
        assert_eq!(pages[0].url, "https://a.example");
        assert_eq!(pages[0].content, "# page a");
    }

    /// extract_highlights posts ids + highlights.maxCharacters and joins the
    /// row's highlights; falls back to page text when no highlights.
    #[tokio::test]
    async fn extract_highlights_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [{
                "url": "https://a.example",
                "highlights": ["highlight one", "highlight two"]
            }],
            "costDollars": { "total": 0.001 }
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .extract_highlights(&http, "exa-hl-key", "https://a.example")
            .await
            .expect("highlights against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/contents", "path: {}", rec.request_line);
        let b = rec.body_json();
        assert_eq!(b["ids"], serde_json::json!(["https://a.example"]));
        assert_eq!(
            b["highlights"]["maxCharacters"], 4000,
            "bounded highlights: {b}"
        );
        assert_eq!(out, "highlight one\nhighlight two");
    }

    /// No highlights on the wire → honest text fallback; neither → URL-class
    /// Unextractable (never a fake HTTP health signal).
    #[tokio::test]
    async fn extract_highlights_falls_back_to_text_then_unextractable() {
        let (base, _rx) = spawn_recording_server(serde_json::json!({
            "results": [{ "url": "https://a.example", "text": "# plain" }]
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .extract_highlights(&http, "exa-hl-key", "https://a.example")
            .await
            .expect("text fallback");
        assert_eq!(out, "# plain");

        let (base, _rx) = spawn_recording_server(serde_json::json!({ "results": [] }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let err = client
            .extract_highlights(&http, "exa-hl-key", "https://a.example")
            .await
            .expect_err("no row");
        match err {
            ProviderError::Unextractable { provider, message } => {
                assert_eq!(provider, "exa");
                assert!(message.contains("no results"), "{message}");
            }
            other => panic!("expected Unextractable, got {other:?}"),
        }
    }
}
