use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serpotter_auth::{authentication_error, extract_token, problem_response};
use serpotter_core::{
    fallback_chain, reciprocal_rank_fusion, route_search, RouteDebug, RouteInput, RrfList,
    SearchQuery, SearchResponse, Strategy,
};
use serpotter_db::{Db, EXPECTED_SCHEMA_VERSION};
use serpotter_keypool::{KeyPool, KeyPoolError};
use serpotter_providers::{
    ProviderError, ProviderRegistry, ProviderSearchParams, ProviderResult, SVC_FIRECRAWL,
    SVC_TAVILY, SVC_XAI,
};

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub keys: Arc<KeyPool>,
    pub providers: ProviderRegistry,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveBody {
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadyBody {
    status: &'static str,
    schema_version: Option<i64>,
    expected: i64,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/live", get(live))
        .route("/ready", get(ready))
        .route("/api/search", post(search))
        .with_state(state)
}

async fn live() -> Json<LiveBody> {
    Json(LiveBody { status: "ok" })
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let expected = EXPECTED_SCHEMA_VERSION;
    match state.db.schema_version().await {
        Ok(version) if version >= expected => (
            StatusCode::OK,
            Json(ReadyBody {
                status: "ok",
                schema_version: Some(version),
                expected,
            }),
        )
            .into_response(),
        Ok(version) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyBody {
                status: "not_ready",
                schema_version: Some(version),
                expected,
            }),
        )
            .into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyBody {
                status: "not_ready",
                schema_version: None,
                expected,
            }),
        )
            .into_response(),
    }
}

async fn search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SearchQuery>,
) -> impl IntoResponse {
    let Some(token) = extract_token(&headers) else {
        return authentication_error("Missing API token");
    };

    match state.db.get_token_by_value(&token).await {
        Ok(Some(_)) => {}
        Ok(None) => return authentication_error("Invalid token"),
        Err(_) => {
            return problem_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "DatabaseError",
                "Token lookup failed",
            );
        }
    }

    if body.query.trim().is_empty() {
        return problem_response(StatusCode::BAD_REQUEST, "ValidationError", "missing_query");
    }

    let decision = route_search(RouteInput { query: &body });
    let max_results = body.clamped_max_results();
    let include_content = body.include_content.unwrap_or(false);

    let include_domains = body
        .include_domains
        .as_ref()
        .map(|v| v.as_list())
        .unwrap_or_default();
    let exclude_domains = body
        .exclude_domains
        .as_ref()
        .map(|v| v.as_list())
        .unwrap_or_default();

    let result = if decision.hybrid {
        execute_hybrid(&state, &body, &decision, max_results, include_content, &include_domains, &exclude_domains).await
    } else if decision.blend {
        execute_blend(&state, &body, &decision, max_results, include_content, &include_domains, &exclude_domains).await
    } else {
        execute_single_chain(&state, &body, &decision, max_results, include_content, &include_domains, &exclude_domains).await
    };

    match result {
        Ok(mut resp) => {
            resp.route_debug = Some(RouteDebug {
                intent: Some(decision.intent.clone()),
                strategy: Some(decision.strategy.as_str().into()),
                reason: Some(decision.reason.clone()),
            });
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(SearchExecError::NoHealthyKey(msg)) => {
            problem_response(StatusCode::SERVICE_UNAVAILABLE, "NoHealthyKey", msg)
        }
        Err(SearchExecError::Provider(msg)) => {
            problem_response(StatusCode::BAD_GATEWAY, "ProviderError", msg)
        }
        Err(SearchExecError::Search(msg)) => {
            problem_response(StatusCode::BAD_GATEWAY, "SearchError", msg)
        }
        Err(SearchExecError::Db(msg)) => {
            problem_response(StatusCode::INTERNAL_SERVER_ERROR, "DatabaseError", msg)
        }
    }
}

enum SearchExecError {
    NoHealthyKey(String),
    Provider(String),
    Search(String),
    Db(String),
}

async fn execute_single_chain(
    state: &AppState,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<SearchResponse, SearchExecError> {
    let chain = fallback_chain(&decision.provider);
    let mut last_err = SearchExecError::NoHealthyKey("No healthy provider key".into());

    for provider in chain {
        match run_provider(
            state,
            provider,
            body,
            decision,
            max_results,
            include_content,
            include_domains,
            exclude_domains,
            decision.sources.as_deref(),
        )
        .await
        {
            Ok(r) => {
                return Ok(r.into_search_response());
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn execute_hybrid(
    state: &AppState,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<SearchResponse, SearchExecError> {
    let web_src = ["web".to_string()];
    let x_src = ["x".to_string()];
    let web = run_provider(
        state,
        SVC_TAVILY,
        body,
        decision,
        max_results,
        include_content,
        include_domains,
        exclude_domains,
        Some(web_src.as_slice()),
    )
    .await;
    let x_max = max_results.min(5);
    let x = run_provider(
        state,
        SVC_XAI,
        body,
        decision,
        x_max,
        false,
        include_domains,
        exclude_domains,
        Some(x_src.as_slice()),
    )
    .await;

    let web_items = web.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let x_items = x.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    if web_items.is_empty() && x_items.is_empty() {
        return Err(web.err().or(x.err()).unwrap_or(SearchExecError::Search(
            "hybrid both legs empty".into(),
        )));
    }
    let merged = reciprocal_rank_fusion(&[
        RrfList {
            items: web_items,
            weight: 1.0,
        },
        RrfList {
            items: x_items,
            weight: 0.7,
        },
    ]);
    let items: Vec<_> = merged.into_iter().take(max_results as usize).collect();
    Ok(SearchResponse {
        query: body.query.clone(),
        provider_used: "hybrid".into(),
        items,
        answer: web.ok().and_then(|r| r.answer),
        route_debug: None,
    })
}

async fn execute_blend(
    state: &AppState,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
) -> Result<SearchResponse, SearchExecError> {
    let primary = decision.provider.as_str();
    let secondary = if primary == SVC_FIRECRAWL {
        SVC_TAVILY
    } else {
        SVC_FIRECRAWL
    };

    let a = run_provider(
        state,
        primary,
        body,
        decision,
        max_results,
        include_content,
        include_domains,
        exclude_domains,
        None,
    )
    .await;
    let b = run_provider(
        state,
        secondary,
        body,
        decision,
        max_results,
        include_content,
        include_domains,
        exclude_domains,
        None,
    )
    .await;

    // verify: also try exa
    let c = if decision.strategy == Strategy::Verify {
        Some(
            run_provider(
                state,
                "exa",
                body,
                decision,
                max_results,
                include_content,
                include_domains,
                exclude_domains,
                None,
            )
            .await,
        )
    } else {
        None
    };

    let a_items = a.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let b_items = b.as_ref().map(|r| r.items.as_slice()).unwrap_or(&[]);
    let c_items = c
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .map(|r| r.items.as_slice())
        .unwrap_or(&[]);

    if a_items.is_empty() && b_items.is_empty() && c_items.is_empty() {
        return Err(a.err().or(b.err()).unwrap_or(SearchExecError::Search(
            "blend empty".into(),
        )));
    }

    let mut lists = vec![
        RrfList {
            items: a_items,
            weight: 1.0,
        },
        RrfList {
            items: b_items,
            weight: 0.7,
        },
    ];
    if !c_items.is_empty() {
        lists.push(RrfList {
            items: c_items,
            weight: 0.7,
        });
    }
    let merged = reciprocal_rank_fusion(&lists);
    let items: Vec<_> = merged.into_iter().take(max_results as usize).collect();
    let answer = a.ok().and_then(|r| r.answer);
    Ok(SearchResponse {
        query: body.query.clone(),
        provider_used: if decision.strategy == Strategy::Verify {
            "blend-verify".into()
        } else {
            "blend".into()
        },
        items,
        answer,
        route_debug: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_provider(
    state: &AppState,
    provider: &str,
    body: &SearchQuery,
    decision: &serpotter_core::RouteDecision,
    max_results: u32,
    include_content: bool,
    include_domains: &[String],
    exclude_domains: &[String],
    sources_override: Option<&[String]>,
) -> Result<ProviderResult, SearchExecError> {
    let lease = match state.keys.acquire(provider).await {
        Ok(l) => l,
        Err(KeyPoolError::NoHealthyKey(s)) => {
            return Err(SearchExecError::NoHealthyKey(format!(
                "No healthy {s} key"
            )));
        }
        Err(KeyPoolError::Db(e)) => {
            return Err(SearchExecError::Db(e.to_string()));
        }
    };

    let sources = sources_override.or(decision.sources.as_deref());
    let params = ProviderSearchParams {
        query: body.query.trim(),
        max_results,
        api_key: &lease.key,
        include_content,
        include_answer: true,
        search_depth: body.search_depth.as_deref(),
        tavily_topic: decision.tavily_topic.as_deref(),
        firecrawl_categories: decision.firecrawl_categories.as_deref(),
        sources,
        include_domains: if include_domains.is_empty() {
            None
        } else {
            Some(include_domains)
        },
        exclude_domains: if exclude_domains.is_empty() {
            None
        } else {
            Some(exclude_domains)
        },
        time_range: body.time_range.as_deref(),
        country: body.country.as_deref(),
        exact_match: body.exact_match,
    };

    match state.providers.search(provider, params).await {
        Ok(r) => {
            let _ = state.keys.report_success(lease.id).await;
            Ok(r)
        }
        Err(ProviderError::Upstream { status, body, .. }) if status == 429 || (500..600).contains(&status) => {
            let _ = state.keys.report_failure(lease.id).await;
            Err(SearchExecError::Provider(format!(
                "{provider} upstream {status}: {body}"
            )))
        }
        Err(ProviderError::Upstream { status, body, .. }) => {
            if status == 401 || status == 403 {
                let _ = state.keys.report_failure(lease.id).await;
            }
            Err(SearchExecError::Provider(format!(
                "{provider} upstream {status}: {body}"
            )))
        }
        Err(ProviderError::Http(e)) => {
            let _ = state.keys.report_failure(lease.id).await;
            Err(SearchExecError::Search(format!(
                "{provider} request failed: {e}"
            )))
        }
    }
}
