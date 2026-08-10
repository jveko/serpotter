use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchItem {
    pub title: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub query: String,
    pub provider_used: String,
    pub items: Vec<SearchItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Soft-merge detail when hybrid (or multi-leg) keeps results but a leg failed.
    /// Omitted when all contributing legs succeeded or both legs empty (hard error path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leg_errors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route_debug: Option<RouteDebug>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RouteDebug {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchQuery {
    pub query: String,
    pub max_results: Option<u32>,
    pub mode: Option<String>,
    pub intent: Option<String>,
    pub strategy: Option<String>,
    pub provider: Option<String>,
    pub sources: Option<Sources>,
    pub include_content: Option<bool>,
    pub include_domains: Option<VecOrOne>,
    pub exclude_domains: Option<VecOrOne>,
    pub allowed_x_handles: Option<VecOrOne>,
    pub excluded_x_handles: Option<VecOrOne>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
    pub search_depth: Option<String>,
    pub time_range: Option<String>,
    pub country: Option<String>,
    pub exact_match: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Sources {
    One(String),
    Many(Vec<String>),
}

impl Sources {
    pub fn as_list(&self) -> Vec<String> {
        match self {
            Sources::One(s) => vec![s.clone()],
            Sources::Many(v) => v.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum VecOrOne {
    One(String),
    Many(Vec<String>),
}

impl VecOrOne {
    pub fn as_list(&self) -> Vec<String> {
        match self {
            VecOrOne::One(s) => vec![s.clone()],
            VecOrOne::Many(v) => v.clone(),
        }
    }

    pub fn is_nonempty(&self) -> bool {
        match self {
            VecOrOne::One(s) => !s.is_empty(),
            VecOrOne::Many(v) => !v.is_empty(),
        }
    }
}

impl SearchQuery {
    pub fn clamped_max_results(&self) -> u32 {
        self.max_results.unwrap_or(5).clamp(1, 20)
    }
}
