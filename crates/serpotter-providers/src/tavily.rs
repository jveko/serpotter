use crate::{
    parse_tavily_usage, CreditSnapshot, ExtractResult, ProviderError, ProviderResult,
    ProviderSearchParams,
};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

const DEFAULT: &str = "https://api.tavily.com";

#[derive(Clone)]
pub struct TavilyClient {
    http: Client,
    base_url: String,
}

impl TavilyClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::new_with_proxy(base_url, None)
    }

    pub fn new_with_proxy(base_url: impl Into<String>, proxy_url: Option<&str>) -> Self {
        Self {
            http: crate::http::build_http(proxy_url),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn with_default() -> Self {
        Self::new(DEFAULT)
    }

    pub async fn search(
        &self,
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
        if let Some(tr) = p.time_range {
            body["time_range"] = serde_json::json!(tr);
        }
        if let Some(c) = p.country {
            body["country"] = serde_json::json!(c);
        }
        if let Some(e) = p.exact_match {
            body["exact_match"] = serde_json::json!(e);
        }

        let res = self
            .http
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
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        let endpoint = format!("{}/extract", self.base_url);
        let body = serde_json::json!({
            "api_key": api_key,
            "urls": [url],
        });
        let res = self
            .http
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
    pub async fn fetch_usage(&self, api_key: &str) -> Result<CreditSnapshot, ProviderError> {
        let url = format!("{}/usage", self.base_url);
        let res = self
            .http
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
