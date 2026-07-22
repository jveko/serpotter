//! Search providers: tavily, firecrawl, exa, xai.

mod firecrawl;
mod exa;
mod tavily;
mod usage;
mod xai;

pub use firecrawl::FirecrawlClient;
pub use exa::ExaClient;
pub use tavily::TavilyClient;
pub use usage::{parse_firecrawl_usage, parse_tavily_usage, CreditSnapshot};
pub use xai::XaiClient;

use serpotter_core::{SearchItem, SearchResponse};
use thiserror::Error;

pub const SVC_TAVILY: &str = "tavily";
pub const SVC_FIRECRAWL: &str = "firecrawl";
pub const SVC_EXA: &str = "exa";
pub const SVC_XAI: &str = "xai";

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("upstream {provider} status {status}: {body}")]
    Upstream {
        provider: String,
        status: u16,
        body: String,
    },
}

#[derive(Debug, Clone)]
pub struct ProviderSearchParams<'a> {
    pub query: &'a str,
    pub max_results: u32,
    pub api_key: &'a str,
    pub include_content: bool,
    pub include_answer: bool,
    pub search_depth: Option<&'a str>,
    pub tavily_topic: Option<&'a str>,
    pub firecrawl_categories: Option<&'a [String]>,
    pub sources: Option<&'a [String]>,
    pub include_domains: Option<&'a [String]>,
    pub exclude_domains: Option<&'a [String]>,
    pub time_range: Option<&'a str>,
    pub country: Option<&'a str>,
    pub exact_match: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ProviderResult {
    pub provider: String,
    pub query: String,
    pub items: Vec<SearchItem>,
    pub answer: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExtractResult {
    pub url: String,
    pub title: Option<String>,
    pub content: String,
    pub provider: String,
}

impl ProviderResult {
    pub fn into_search_response(self) -> SearchResponse {
        SearchResponse {
            query: self.query,
            provider_used: self.provider,
            items: self.items,
            answer: self.answer,
            route_debug: None,
        }
    }
}

#[derive(Clone)]
pub struct ProviderRegistry {
    pub tavily: TavilyClient,
    pub firecrawl: FirecrawlClient,
    pub exa: ExaClient,
    pub xai: XaiClient,
}

impl ProviderRegistry {
    /// Direct egress (no commercial CONNECT).
    pub fn from_env() -> Self {
        Self::with_proxy_url(None)
    }

    /// `proxy_url` is `http://[user:pass@]host:port` for Tavily/Firecrawl/Exa.
    /// xAI is always direct (mysearch parity).
    pub fn with_proxy_url(proxy_url: Option<&str>) -> Self {
        Self {
            tavily: TavilyClient::new_with_proxy(
                std::env::var("TAVILY_BASE_URL")
                    .unwrap_or_else(|_| "https://api.tavily.com".into()),
                proxy_url,
            ),
            firecrawl: FirecrawlClient::new_with_proxy(
                std::env::var("FIRECRAWL_BASE_URL")
                    .unwrap_or_else(|_| "https://api.firecrawl.dev".into()),
                proxy_url,
            ),
            exa: ExaClient::new_with_proxy(
                std::env::var("EXA_BASE_URL").unwrap_or_else(|_| "https://api.exa.ai".into()),
                proxy_url,
            ),
            xai: XaiClient::new(
                std::env::var("XAI_BASE_URL").unwrap_or_else(|_| "https://api.x.ai/v1".into()),
            ),
        }
    }

    pub async fn search(
        &self,
        provider: &str,
        params: ProviderSearchParams<'_>,
    ) -> Result<ProviderResult, ProviderError> {
        match provider {
            SVC_TAVILY => self.tavily.search(params).await,
            SVC_FIRECRAWL => self.firecrawl.search(params).await,
            SVC_EXA => self.exa.search(params).await,
            SVC_XAI => self.xai.search(params).await,
            other => Err(ProviderError::Upstream {
                provider: other.into(),
                status: 400,
                body: format!("unknown provider {other}"),
            }),
        }
    }

    pub async fn extract(
        &self,
        provider: &str,
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        match provider {
            SVC_FIRECRAWL => self.firecrawl.extract(url, api_key).await,
            SVC_TAVILY => self.tavily.extract(url, api_key).await,
            other => Err(ProviderError::Upstream {
                provider: other.into(),
                status: 400,
                body: format!("extract not supported for {other}"),
            }),
        }
    }
}
