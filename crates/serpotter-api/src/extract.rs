//! POST /api/extract and POST /api/research.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serpotter_auth::problem_response;
use serpotter_core::{SearchItem, SearchQuery};
use serpotter_keypool::KeyPoolError;
use serpotter_providers::{
    ExtractResult, ProviderError, SVC_FIRECRAWL, SVC_TAVILY,
};

use crate::search::{search_inner, SearchExecError};
use crate::{require_api_token, AppState};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    pub url: String,
    /// Optional force provider: firecrawl | tavily
    pub provider: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResponse {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    pub provider_used: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchRequest {
    pub query: String,
    /// mysearch REST: webMaxResults. Aliases: maxResults.
    #[serde(default, alias = "maxResults", alias = "max_results")]
    pub web_max_results: Option<u32>,
    /// mysearch REST/MCP: scrapeTopN / scrape_top_n. Aliases: extractTopN.
    #[serde(default, alias = "extractTopN", alias = "extract_top_n", alias = "scrape_top_n")]
    pub scrape_top_n: Option<u32>,
    pub include_content: Option<bool>,
    /// mysearch: socialMaxResults (0 = skip social).
    #[serde(default, alias = "social_max_results")]
    pub social_max_results: Option<u32>,
}

/// Live wire matches mysearch ResearchResult camelCase (encodeKeys not applied at HTTP).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchResponse {
    pub query: String,
    pub web_results: Vec<SearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub social_results: Option<Vec<SearchItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scraped_pages: Option<Vec<ScrapedPage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<Citation>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrapedPage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Citation {
    pub title: String,
    pub url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers_consulted: Option<Vec<String>>,
}

pub async fn extract_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExtractRequest>,
) -> impl IntoResponse {
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }
    if body.url.trim().is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_url");
    }

    match extract_url(&state, body.url.trim(), body.provider.as_deref()).await {
        Ok(r) => (StatusCode::OK, Json(r)).into_response(),
        Err(ExtractError::NoHealthyKey(m)) => {
            problem_response(StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey", m)
        }
        Err(ExtractError::Provider(m)) => {
            problem_response(StatusCode::BAD_GATEWAY, "ProviderError", m)
        }
        Err(ExtractError::Db(m)) => {
            problem_response(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", m)
        }
    }
}

pub async fn research_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ResearchRequest>,
) -> impl IntoResponse {
    if let Err(r) = require_api_token(&state, &headers).await {
        return r;
    }
    if body.query.trim().is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_query");
    }

    match research_inner(&state, body).await {
        Ok(r) => (StatusCode::OK, Json(r)).into_response(),
        Err(ResearchError::Search(SearchExecError::NoHealthyKey(m)))
        | Err(ResearchError::Extract(ExtractError::NoHealthyKey(m))) => {
            problem_response(StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey", m)
        }
        Err(ResearchError::Search(SearchExecError::Provider(m)))
        | Err(ResearchError::Search(SearchExecError::Search(m)))
        | Err(ResearchError::Extract(ExtractError::Provider(m))) => {
            problem_response(StatusCode::BAD_GATEWAY, "ProviderError", m)
        }
        Err(ResearchError::Search(SearchExecError::Db(m)))
        | Err(ResearchError::Extract(ExtractError::Db(m))) => {
            problem_response(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", m)
        }
    }
}

#[derive(Debug)]
pub enum ExtractError {
    NoHealthyKey(String),
    Provider(String),
    Db(String),
}

#[derive(Debug)]
pub enum ResearchError {
    Search(SearchExecError),
    #[allow(dead_code)]
    Extract(ExtractError),
}

pub async fn extract_url(
    state: &AppState,
    url: &str,
    preferred: Option<&str>,
) -> Result<ExtractResponse, ExtractError> {
    let chain: Vec<&str> = match preferred {
        Some("tavily") => vec![SVC_TAVILY, SVC_FIRECRAWL],
        Some("firecrawl") | None => vec![SVC_FIRECRAWL, SVC_TAVILY],
        Some(other) => {
            return Err(ExtractError::Provider(format!(
                "unknown extract provider {other}"
            )));
        }
    };

    let mut last = ExtractError::NoHealthyKey("No healthy extract key".into());
    for provider in chain {
        match try_extract_provider(state, provider, url).await {
            Ok(r) => return Ok(to_response(r)),
            Err(e) => last = e,
        }
    }
    Err(last)
}

async fn try_extract_provider(
    state: &AppState,
    provider: &str,
    url: &str,
) -> Result<ExtractResult, ExtractError> {
    let batch = match state.keys.acquire_batch(provider, 3).await {
        Ok(b) => b,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(ExtractError::NoHealthyKey(format!("No healthy {s} key")));
        }
        Err(KeyPoolError::Db(e)) => return Err(ExtractError::Db(e.to_string())),
    };

    let mut last = ExtractError::Provider(format!("{provider}: all keys failed"));
    for lease in batch {
        match state.providers.extract(provider, url, &lease.key).await {
            Ok(r) => {
                let _ = state.keys.report_success(lease.id).await;
                return Ok(r);
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if crate::search::is_exhausted_status(provider, status) => {
                let _ = state.keys.report_exhausted(lease.id).await;
                last = ExtractError::Provider(format!(
                    "{provider} exhausted status {status}: {b}"
                ));
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) if status == 401
                || status == 403
                || status == 429
                || (500..600).contains(&status) =>
            {
                let _ = state.keys.report_failure(lease.id).await;
                last = ExtractError::Provider(format!("{provider} upstream {status}: {b}"));
            }
            Err(ProviderError::Upstream {
                status, body: b, ..
            }) => {
                return Err(ExtractError::Provider(format!(
                    "{provider} upstream {status}: {b}"
                )));
            }
            Err(ProviderError::Http(e)) => {
                let _ = state.keys.report_failure(lease.id).await;
                last = ExtractError::Provider(format!("{provider} request failed: {e}"));
            }
        }
    }
    Err(last)
}

fn to_response(r: ExtractResult) -> ExtractResponse {
    ExtractResponse {
        url: r.url,
        title: r.title,
        content: r.content,
        provider_used: r.provider,
    }
}

pub async fn research_inner(
    state: &AppState,
    body: ResearchRequest,
) -> Result<ResearchResponse, ResearchError> {
    let max_results = body.web_max_results.unwrap_or(5).clamp(1, 20);
    // MCP default scrape_top_n=1; REST lean default 2
    let extract_n = body.scrape_top_n.unwrap_or(2).clamp(0, 10) as usize;
    let q = SearchQuery {
        query: body.query.clone(),
        max_results: Some(max_results),
        include_content: body.include_content.or(Some(false)),
        ..Default::default()
    };
    let search = search_inner(state, q)
        .await
        .map_err(ResearchError::Search)?;

    let mut scraped_pages = Vec::new();
    let mut citations = Vec::new();
    for item in &search.items {
        if !item.url.is_empty() {
            citations.push(Citation {
                title: item.title.clone(),
                url: item.url.clone(),
            });
        }
    }

    for item in search.items.iter().take(extract_n) {
        if item.url.is_empty() {
            continue;
        }
        match extract_url(state, &item.url, None).await {
            Ok(e) => {
                let excerpt = e.content.chars().take(280).collect::<String>();
                scraped_pages.push(ScrapedPage {
                    title: e.title,
                    url: e.url,
                    content: Some(e.content),
                    excerpt: Some(excerpt),
                    error: None,
                });
            }
            Err(err) => {
                scraped_pages.push(ScrapedPage {
                    title: Some(item.title.clone()),
                    url: item.url.clone(),
                    content: None,
                    excerpt: None,
                    error: Some(format!("{err:?}")),
                });
            }
        }
    }

    let providers_consulted = {
        let mut p = vec![search.provider_used.clone()];
        p.sort();
        p.dedup();
        p
    };

    Ok(ResearchResponse {
        query: body.query,
        web_results: search.items,
        social_results: None,
        scraped_pages: if scraped_pages.is_empty() {
            None
        } else {
            Some(scraped_pages)
        },
        citations: if citations.is_empty() {
            None
        } else {
            Some(citations)
        },
        evidence: Some(Evidence {
            summary: search.answer,
            providers_consulted: Some(providers_consulted),
        }),
    })
}

// silence unused import if Router not needed
#[allow(dead_code)]
fn _router_ty() {
    let _: Option<Router> = None;
}
