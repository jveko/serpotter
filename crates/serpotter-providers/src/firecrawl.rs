use crate::{ProviderError, ProviderResult, ProviderSearchParams};
use reqwest::Client;
use serde::Deserialize;
use serpotter_core::SearchItem;

#[derive(Clone)]
pub struct FirecrawlClient {
    http: Client,
    base_url: String,
}

impl FirecrawlClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn search(
        &self,
        p: ProviderSearchParams<'_>,
    ) -> Result<ProviderResult, ProviderError> {
        let url = format!("{}/v2/search", self.base_url);
        let sources = p
            .sources
            .map(|s| s.to_vec())
            .unwrap_or_else(|| vec!["web".into()]);
        let mut body = serde_json::json!({
            "query": p.query,
            "limit": p.max_results,
            "sources": sources,
        });
        if let Some(cats) = p.firecrawl_categories {
            if !cats.is_empty() {
                body["categories"] = serde_json::json!(cats);
            }
        }
        if let Some(tr) = p.time_range {
            let tbs = match tr {
                "day" => "qdr:d",
                "week" => "qdr:w",
                "month" => "qdr:m",
                "year" => "qdr:y",
                other => other,
            };
            body["tbs"] = serde_json::json!(tbs);
        }
        if let Some(c) = p.country {
            body["country"] = serde_json::json!(c);
        }
        if p.include_content {
            body["scrapeOptions"] = serde_json::json!({
                "formats": ["markdown"],
                "onlyMainContent": true,
            });
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
                provider: "firecrawl".into(),
                status: status.as_u16(),
                body: text,
            });
        }

        #[derive(Deserialize)]
        struct Up {
            data: Option<Data>,
        }
        #[derive(Deserialize)]
        struct Data {
            web: Option<Vec<Web>>,
            news: Option<Vec<News>>,
        }
        #[derive(Deserialize)]
        struct Web {
            title: Option<String>,
            url: Option<String>,
            description: Option<String>,
            markdown: Option<String>,
        }
        #[derive(Deserialize)]
        struct News {
            title: Option<String>,
            url: Option<String>,
            snippet: Option<String>,
            description: Option<String>,
            markdown: Option<String>,
            date: Option<String>,
        }

        let up: Up = res.json().await?;
        let data = up.data.unwrap_or(Data {
            web: None,
            news: None,
        });
        let mut items = Vec::new();
        for r in data.web.unwrap_or_default() {
            let snippet = r.description.or(r.markdown.clone());
            items.push(SearchItem {
                title: r.title.unwrap_or_default(),
                url: r.url.unwrap_or_default(),
                snippet,
                content: if p.include_content { r.markdown } else { None },
                score: None,
                published: None,
                author: None,
                provider: Some("firecrawl".into()),
                source: Some("web".into()),
            });
        }
        for r in data.news.unwrap_or_default() {
            let snippet = r.snippet.or(r.description).or(r.markdown.clone());
            items.push(SearchItem {
                title: r.title.unwrap_or_default(),
                url: r.url.unwrap_or_default(),
                snippet,
                content: if p.include_content { r.markdown } else { None },
                score: None,
                published: r.date,
                author: None,
                provider: Some("firecrawl".into()),
                source: Some("news".into()),
            });
        }

        Ok(ProviderResult {
            provider: "firecrawl".into(),
            query: p.query.to_string(),
            items,
            answer: None,
        })
    }
}
