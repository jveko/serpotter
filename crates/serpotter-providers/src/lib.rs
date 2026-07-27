//! Search providers: tavily, firecrawl, exa, xai.

mod http;
mod firecrawl;
mod exa;
mod tavily;
mod usage;
mod xai;

pub use firecrawl::FirecrawlClient;
pub use exa::ExaClient;
pub use http::{is_tunnel_error, try_build_http, ClientCache};
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
    /// Page not extractable (empty/failed extract body). Not an HTTP health signal —
    /// product must release holds and continue the extract chain without fail@3.
    #[error("{provider} unextractable: {message}")]
    Unextractable {
        provider: String,
        message: String,
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
    pub allowed_x_handles: Option<&'a [String]>,
    pub excluded_x_handles: Option<&'a [String]>,
    pub from_date: Option<&'a str>,
    pub to_date: Option<&'a str>,
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
            leg_errors: None,
            route_debug: None,
        }
    }
}

/// Dispatches search/extract. Web providers use [`ClientCache`] per-call proxy;
/// xAI always dials direct and **ignores** `proxy`.
#[derive(Clone)]
pub struct ProviderRegistry {
    pub tavily: TavilyClient,
    pub firecrawl: FirecrawlClient,
    pub exa: ExaClient,
    pub xai: XaiClient,
    clients: ClientCache,
}

impl ProviderRegistry {
    /// Direct egress for web providers until a per-call proxy is supplied.
    pub fn from_env() -> Self {
        Self {
            tavily: TavilyClient::new(
                std::env::var("TAVILY_BASE_URL")
                    .unwrap_or_else(|_| "https://api.tavily.com".into()),
            ),
            firecrawl: FirecrawlClient::new(
                std::env::var("FIRECRAWL_BASE_URL")
                    .unwrap_or_else(|_| "https://api.firecrawl.dev".into()),
            ),
            exa: ExaClient::new(
                std::env::var("EXA_BASE_URL").unwrap_or_else(|_| "https://api.exa.ai".into()),
            ),
            xai: XaiClient::new(
                std::env::var("XAI_BASE_URL").unwrap_or_else(|_| "https://api.x.ai/v1".into()),
            ),
            clients: ClientCache::new(),
        }
    }

    /// Build a registry with explicit base URLs (tests: `127.0.0.1:9`).
    pub fn with_clients(
        tavily: TavilyClient,
        firecrawl: FirecrawlClient,
        exa: ExaClient,
        xai: XaiClient,
    ) -> Self {
        Self {
            tavily,
            firecrawl,
            exa,
            xai,
            clients: ClientCache::new(),
        }
    }

    /// Shared direct client (credit sync / admin).
    pub fn direct_client(&self) -> reqwest::Client {
        self.clients.direct()
    }

    /// Resolve or build a cached client for `proxy` (hard-err on bad URL when Some).
    pub fn client_for(&self, proxy: Option<&str>) -> Result<reqwest::Client, ProviderError> {
        self.clients.client_for(proxy)
    }

    pub async fn search(
        &self,
        provider: &str,
        params: ProviderSearchParams<'_>,
        proxy: Option<&str>,
    ) -> Result<ProviderResult, ProviderError> {
        match provider {
            SVC_XAI => {
                // xAI always direct — never touch proxy cache / Proxy::all.
                let _ = proxy;
                self.xai.search(params).await
            }
            SVC_TAVILY => {
                let http = self.clients.client_for(proxy)?;
                self.tavily.search(&http, params).await
            }
            SVC_FIRECRAWL => {
                let http = self.clients.client_for(proxy)?;
                self.firecrawl.search(&http, params).await
            }
            SVC_EXA => {
                let http = self.clients.client_for(proxy)?;
                self.exa.search(&http, params).await
            }
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
        proxy: Option<&str>,
    ) -> Result<ExtractResult, ProviderError> {
        let http = self.clients.client_for(proxy)?;
        match provider {
            SVC_FIRECRAWL => self.firecrawl.extract(&http, url, api_key).await,
            SVC_TAVILY => self.tavily.extract(&http, url, api_key).await,
            other => Err(ProviderError::Upstream {
                provider: other.into(),
                status: 400,
                body: format!("extract not supported for {other}"),
            }),
        }
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn dummy_params(key: &str) -> ProviderSearchParams<'_> {
        ProviderSearchParams {
            query: "q",
            max_results: 1,
            api_key: key,
            include_content: false,
            include_answer: false,
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
        }
    }

    #[test]
    fn client_for_same_url_cached() {
        let reg = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let _a = reg
            .client_for(Some("http://proxy.example:8080"))
            .expect("a");
        let _b = reg
            .client_for(Some("http://proxy.example:8080"))
            .expect("b");
        assert_eq!(reg.clients.cache_len(), 1);
    }

    #[test]
    fn web_search_bad_proxy_errors_before_network() {
        let reg = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(reg.search(
                SVC_TAVILY,
                dummy_params("k"),
                Some("not-a-url-:::"),
            ))
            .expect_err("hard fail");
        assert!(
            matches!(err, ProviderError::Http(_)),
            "expected Http proxy build error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn xai_search_ignores_bad_proxy() {
        // Invalid proxy must not short-circuit as Proxy::all Err — xAI ignores proxy.
        let reg = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let err = reg
            .search(SVC_XAI, dummy_params("k"), Some("not-a-url-:::"))
            .await
            .expect_err("network to :9");
        // Connection refused / timeout — not a proxy-parse Http that happens before send
        // for web providers. Cache must stay empty.
        assert_eq!(reg.clients.cache_len(), 0);
        match err {
            ProviderError::Http(e) => {
                // Direct connect fail, not "builder failed for proxy"
                let s = e.to_string();
                assert!(
                    !s.contains("builder") || e.is_connect() || e.is_request() || e.is_timeout(),
                    "unexpected err: {s}"
                );
            }
            ProviderError::Upstream { .. } => {
                // Unreachable host might still surface oddly; ok
            }
            ProviderError::Unextractable { .. } => {
                panic!("search must not yield Unextractable");
            }
        }
        let _ = reg.xai.http_client();
    }
}
