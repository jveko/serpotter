use crate::{ProviderError, ProviderResult, ProviderSearchParams};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

#[derive(Clone)]
pub struct ExaClient {
    http: Client,
    base_url: String,
}

impl ExaClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::new_with_proxy(base_url, None)
    }

    pub fn new_with_proxy(base_url: impl Into<String>, proxy_url: Option<&str>) -> Self {
        Self {
            http: crate::http::build_http(proxy_url),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn search(
        &self,
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

        let res = self
            .http
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
        })
    }
}
