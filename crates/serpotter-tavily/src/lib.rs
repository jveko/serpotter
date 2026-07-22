//! Tavily search provider (body `api_key` auth).

use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const DEFAULT_BASE_URL: &str = "https://api.tavily.com";
pub const SERVICE: &str = "tavily";

#[derive(Debug, Error)]
pub enum TavilyError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstream status {status}: {body}")]
    Upstream { status: u16, body: String },
}

#[derive(Debug, Clone)]
pub struct TavilyClient {
    http: Client,
    base_url: String,
}

#[derive(Debug, Clone)]
pub struct TavilySearchParams<'a> {
    pub query: &'a str,
    pub max_results: u32,
    pub api_key: &'a str,
    pub search_depth: &'a str,
    pub include_answer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub query: String,
    pub provider_used: String,
    pub items: Vec<SearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TavilyUpstream {
    query: Option<String>,
    answer: Option<String>,
    results: Option<Vec<TavilyResult>>,
}

#[derive(Debug, Deserialize)]
struct TavilyResult {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
    score: Option<f64>,
}

impl TavilyClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn with_default_url() -> Self {
        Self::new(DEFAULT_BASE_URL)
    }

    pub async fn search(&self, params: TavilySearchParams<'_>) -> Result<SearchResponse, TavilyError> {
        let url = format!("{}/search", self.base_url);
        let body = serde_json::json!({
            "api_key": params.api_key,
            "query": params.query,
            "max_results": params.max_results,
            "search_depth": params.search_depth,
            "topic": "general",
            "include_answer": params.include_answer,
            "include_raw_content": false,
        });

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
            return Err(TavilyError::Upstream {
                status: status.as_u16(),
                body: text,
            });
        }

        let upstream: TavilyUpstream = res.json().await?;
        let items = upstream
            .results
            .unwrap_or_default()
            .into_iter()
            .map(|r| SearchItem {
                title: r.title.unwrap_or_default(),
                url: r.url.unwrap_or_default(),
                snippet: r.content,
                score: r.score,
                provider: Some("tavily".into()),
                source: Some("web".into()),
            })
            .collect();

        let answer = upstream.answer.filter(|a| !a.is_empty());

        Ok(SearchResponse {
            query: upstream.query.unwrap_or_else(|| params.query.to_string()),
            provider_used: "tavily".into(),
            items,
            answer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_response_serializes_camel_case() {
        let r = SearchResponse {
            query: "q".into(),
            provider_used: "tavily".into(),
            items: vec![SearchItem {
                title: "t".into(),
                url: "https://e".into(),
                snippet: Some("s".into()),
                score: Some(0.9),
                provider: Some("tavily".into()),
                source: Some("web".into()),
            }],
            answer: Some("a".into()),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["providerUsed"], "tavily");
        assert_eq!(v["items"][0]["title"], "t");
        assert!(v.get("provider_used").is_none());
    }
}
