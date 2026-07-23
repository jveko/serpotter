use crate::{ProviderError, ProviderResult, ProviderSearchParams};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use serpotter_core::SearchItem;

/// xAI always dials direct — never uses the commercial proxy client cache.
#[derive(Clone)]
pub struct XaiClient {
    http: Client,
    base_url: String,
    model: String,
}

impl XaiClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: crate::http::build_direct(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: std::env::var("XAI_MODEL").unwrap_or_else(|_| "grok-4.3".into()),
        }
    }

    /// Exposed for tests asserting xAI never swaps onto a proxied client.
    pub fn http_client(&self) -> &Client {
        &self.http
    }

    pub async fn search(
        &self,
        p: ProviderSearchParams<'_>,
    ) -> Result<ProviderResult, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let wants_x = p
            .sources
            .map(|s| s.iter().any(|x| x == "x"))
            .unwrap_or(false);

        // Official: web_search tool for web; empty tools + X-oriented prompt for social
        // (never emit tools.type=x_search — grok2api rejects it).
        let (tools, prompt) = if wants_x {
            let base = format!(
                "Search X/Twitter for recent posts about: {}. Summarize findings with source URLs.",
                p.query
            );
            let prompt = append_xai_prompt_constraints(
                &base,
                true,
                p.allowed_x_handles,
                p.excluded_x_handles,
                p.from_date,
                p.to_date,
            );
            (json!([]), prompt)
        } else {
            let mut tool = json!({ "type": "web_search" });
            if let Some(d) = p.include_domains {
                if !d.is_empty() {
                    tool["allowed_domains"] = json!(d.iter().take(5).collect::<Vec<_>>());
                }
            }
            if let Some(d) = p.exclude_domains {
                if !d.is_empty() {
                    tool["excluded_domains"] = json!(d.iter().take(5).collect::<Vec<_>>());
                }
            }
            let prompt = append_xai_prompt_constraints(
                p.query,
                false,
                None,
                None,
                p.from_date,
                p.to_date,
            );
            (json!([tool]), prompt)
        };

        let body = json!({
            "model": self.model,
            "input": [{ "role": "user", "content": prompt }],
            "tools": tools,
            "store": false,
        });

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
                provider: "xai".into(),
                status: status.as_u16(),
                body: text,
            });
        }

        #[derive(Deserialize)]
        struct Up {
            output_text: Option<String>,
            citations: Option<Vec<Cit>>,
            output: Option<Vec<OutMsg>>,
        }
        #[derive(Deserialize)]
        struct Cit {
            title: Option<String>,
            url: Option<String>,
        }
        #[derive(Deserialize)]
        struct OutMsg {
            content: Option<Vec<Part>>,
        }
        #[derive(Deserialize)]
        struct Part {
            #[serde(rename = "type")]
            #[allow(dead_code)]
            kind: Option<String>,
            text: Option<String>,
            annotations: Option<Vec<Ann>>,
        }
        #[derive(Deserialize)]
        struct Ann {
            #[serde(rename = "type")]
            kind: Option<String>,
            title: Option<String>,
            url: Option<String>,
        }

        let up: Up = res.json().await?;
        let mut answer = up.output_text.filter(|s| !s.is_empty());
        if answer.is_none() {
            if let Some(msgs) = &up.output {
                let mut buf = String::new();
                for m in msgs {
                    if let Some(parts) = &m.content {
                        for part in parts {
                            if let Some(t) = &part.text {
                                buf.push_str(t);
                            }
                        }
                    }
                }
                if !buf.is_empty() {
                    answer = Some(buf);
                }
            }
        }

        let source = if wants_x { "x" } else { "web" };
        let mut items: Vec<SearchItem> = up
            .citations
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| {
                let url = c.url?;
                Some(SearchItem {
                    title: c.title.unwrap_or_default(),
                    url,
                    snippet: Some(String::new()),
                    content: None,
                    score: None,
                    published: None,
                    author: None,
                    provider: Some("xai".into()),
                    source: Some(source.into()),
                })
            })
            .collect();

        // annotations url_citation
        if let Some(msgs) = up.output {
            for m in msgs {
                if let Some(parts) = m.content {
                    for part in parts {
                        if let Some(anns) = part.annotations {
                            for a in anns {
                                if a.kind.as_deref() == Some("url_citation") {
                                    if let Some(u) = a.url {
                                        items.push(SearchItem {
                                            title: a.title.unwrap_or_default(),
                                            url: u,
                                            snippet: Some(String::new()),
                                            content: None,
                                            score: None,
                                            published: None,
                                            author: None,
                                            provider: Some("xai".into()),
                                            source: Some(source.into()),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(ProviderResult {
            provider: "xai".into(),
            query: p.query.to_string(),
            items,
            answer,
        })
    }
}

/// Append handle/date constraints to an xAI user prompt.
/// Handles only apply on the social (X) path; dates apply to both social and web.
pub(crate) fn append_xai_prompt_constraints(
    base: &str,
    social: bool,
    allowed_x_handles: Option<&[String]>,
    excluded_x_handles: Option<&[String]>,
    from_date: Option<&str>,
    to_date: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if social {
        if let Some(h) = allowed_x_handles {
            if !h.is_empty() {
                let handles = h
                    .iter()
                    .map(|s| normalize_handle(s))
                    .collect::<Vec<_>>()
                    .join(",");
                parts.push(format!("only from {handles}"));
            }
        }
        if let Some(h) = excluded_x_handles {
            if !h.is_empty() {
                let handles = h
                    .iter()
                    .map(|s| normalize_handle(s))
                    .collect::<Vec<_>>()
                    .join(",");
                parts.push(format!("exclude {handles}"));
            }
        }
    }
    match (from_date, to_date) {
        (Some(f), Some(t)) => parts.push(format!("between {f} and {t}")),
        (Some(f), None) => parts.push(format!("from {f}")),
        (None, Some(t)) => parts.push(format!("until {t}")),
        (None, None) => {}
    }
    if parts.is_empty() {
        base.to_string()
    } else {
        format!("{base} ({})", parts.join("; "))
    }
}

fn normalize_handle(s: &str) -> String {
    let t = s.trim();
    if t.starts_with('@') {
        t.to_string()
    } else {
        format!("@{t}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn social_prompt_includes_allowed_handles() {
        let allowed = vec!["elonmusk".into(), "@OpenAI".into()];
        let out = append_xai_prompt_constraints(
            "Search X/Twitter for recent posts about: ai.",
            true,
            Some(&allowed),
            None,
            None,
            None,
        );
        assert!(out.contains("only from @elonmusk,@OpenAI"), "{out}");
    }

    #[test]
    fn social_prompt_includes_excluded_and_dates() {
        let excluded = vec!["spam".into()];
        let out = append_xai_prompt_constraints(
            "base",
            true,
            None,
            Some(&excluded),
            Some("2026-01-01"),
            Some("2026-01-31"),
        );
        assert!(out.contains("exclude @spam"), "{out}");
        assert!(out.contains("between 2026-01-01 and 2026-01-31"), "{out}");
    }

    #[test]
    fn web_prompt_skips_handles_keeps_dates() {
        let allowed = vec!["someone".into()];
        let out = append_xai_prompt_constraints(
            "quantum computing",
            false,
            Some(&allowed),
            None,
            Some("2025-06-01"),
            None,
        );
        assert!(!out.contains("@someone"), "{out}");
        assert!(out.contains("from 2025-06-01"), "{out}");
        assert!(out.starts_with("quantum computing"), "{out}");
    }

    #[test]
    fn no_constraints_returns_base() {
        let out = append_xai_prompt_constraints("plain", true, None, None, None, None);
        assert_eq!(out, "plain");
    }
}
