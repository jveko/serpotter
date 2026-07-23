use crate::{
    parse_tavily_usage, CreditSnapshot, ExtractResult, ProviderError, ProviderResult,
    ProviderSearchParams,
};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

const DEFAULT: &str = "https://api.tavily.com";

/// Thin Tavily adapter — HTTP client is supplied per call (registry cache).
#[derive(Clone)]
pub struct TavilyClient {
    base_url: String,
}

impl TavilyClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn with_default() -> Self {
        Self::new(DEFAULT)
    }

    pub async fn search(
        &self,
        http: &Client,
        p: ProviderSearchParams<'_>,
    ) -> Result<ProviderResult, ProviderError> {
        let url = format!("{}/search", self.base_url);
        let mut body = serde_json::json!({
            "api_key": p.api_key,
            "query": p.query,
            "max_results": p.max_results,
            "search_depth": p.search_depth.unwrap_or("basic"),
            "topic": p.tavily_topic.unwrap_or("general"),
            "include_answer": p.include_answer,
            "include_raw_content": p.include_content,
        });
        if let Some(d) = p.include_domains {
            if !d.is_empty() {
                body["include_domains"] = serde_json::json!(d);
            }
        }
        if let Some(d) = p.exclude_domains {
            if !d.is_empty() {
                body["exclude_domains"] = serde_json::json!(d);
            }
        }
        // Absolute dates win over time_range (Tavily forbids both).
        apply_tavily_date_filters(&mut body, p.from_date, p.to_date, p.time_range);
        if let Some(c) = p.country {
            body["country"] = serde_json::json!(c);
        }
        if let Some(e) = p.exact_match {
            body["exact_match"] = serde_json::json!(e);
        }

        let res = http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            query: Option<String>,
            answer: Option<String>,
            results: Option<Vec<Row>>,
        }
        #[derive(Deserialize)]
        struct Row {
            title: Option<String>,
            url: Option<String>,
            content: Option<String>,
            raw_content: Option<String>,
            score: Option<f64>,
        }
        let up: Up = res.json().await?;
        let items = up
            .results
            .unwrap_or_default()
            .into_iter()
            .map(|r| SearchItem {
                title: r.title.unwrap_or_default(),
                url: r.url.unwrap_or_default(),
                snippet: r.content,
                content: if p.include_content {
                    r.raw_content
                } else {
                    None
                },
                score: r.score,
                published: None,
                author: None,
                provider: Some("tavily".into()),
                source: Some("web".into()),
            })
            .collect();
        Ok(ProviderResult {
            provider: "tavily".into(),
            query: up.query.unwrap_or_else(|| p.query.to_string()),
            items,
            answer: up.answer.filter(|a| !a.is_empty()),
        })
    }

    /// Extract page content via Tavily `/extract`.
    pub async fn extract(
        &self,
        http: &Client,
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        let endpoint = format!("{}/extract", self.base_url);
        let body = serde_json::json!({
            "api_key": api_key,
            "urls": [url],
        });
        let res = http
            .post(&endpoint)
            .header("Content-Type", "application/json")
            .header("User-Agent", "Serpotter/0.1")
            .json(&body)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
            failed_results: Option<Vec<Failed>>,
        }
        #[derive(Deserialize)]
        struct Row {
            url: Option<String>,
            raw_content: Option<String>,
            content: Option<String>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Failed {
            url: Option<String>,
            error: Option<String>,
        }
        let up: Up = res.json().await?;
        if let Some(first) = up.results.unwrap_or_default().into_iter().next() {
            return Ok(ExtractResult {
                url: first.url.unwrap_or_else(|| url.to_string()),
                title: None,
                content: first
                    .raw_content
                    .or(first.content)
                    .unwrap_or_default(),
                provider: "tavily".into(),
            });
        }
        let fail_msg = up
            .failed_results
            .unwrap_or_default()
            .into_iter()
            .next()
            .and_then(|f| f.error)
            .unwrap_or_else(|| "extract returned no results".into());
        Err(ProviderError::Upstream {
            provider: "tavily".into(),
            status: 502,
            body: fail_msg,
        })
    }

    /// Fetch key/account credit usage via `GET /usage`.
    ///
    /// Auth: Bearer header (mysearch parity). Tavily search/extract use body `api_key`;
    /// usage endpoint is GET with `Authorization: Bearer {key}`.
    pub async fn fetch_usage(
        &self,
        http: &Client,
        api_key: &str,
    ) -> Result<CreditSnapshot, ProviderError> {
        let url = format!("{}/usage", self.base_url);
        let res = http
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            let text = res.text().await.unwrap_or_default();
            return Err(ProviderError::Upstream {
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        let v: serde_json::Value = res.json().await?;
        parse_tavily_usage(&v)
    }
}

/// Apply absolute dates or relative time_range to a Tavily search body.
/// Absolute dates take precedence; Tavily forbids sending both.
pub(crate) fn apply_tavily_date_filters(
    body: &mut serde_json::Value,
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
) {
    let has_abs = from_date.is_some() || to_date.is_some();
    if let Some(d) = from_date {
        body["start_date"] = serde_json::json!(d);
    }
    if let Some(d) = to_date {
        body["end_date"] = serde_json::json!(d);
    }
    if !has_abs {
        if let Some(tr) = time_range {
            body["time_range"] = serde_json::json!(tr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_dates_set_start_end_skip_time_range() {
        let mut body = serde_json::json!({});
        apply_tavily_date_filters(
            &mut body,
            Some("2026-01-01"),
            Some("2026-01-31"),
            Some("week"),
        );
        assert_eq!(body["start_date"], "2026-01-01");
        assert_eq!(body["end_date"], "2026-01-31");
        assert!(body.get("time_range").is_none(), "{body}");
    }

    #[test]
    fn only_from_date_skips_time_range() {
        let mut body = serde_json::json!({});
        apply_tavily_date_filters(&mut body, Some("2026-03-01"), None, Some("month"));
        assert_eq!(body["start_date"], "2026-03-01");
        assert!(body.get("end_date").is_none());
        assert!(body.get("time_range").is_none(), "{body}");
    }

    #[test]
    fn time_range_when_no_absolute_dates() {
        let mut body = serde_json::json!({});
        apply_tavily_date_filters(&mut body, None, None, Some("day"));
        assert_eq!(body["time_range"], "day");
        assert!(body.get("start_date").is_none());
        assert!(body.get("end_date").is_none());
    }
}
