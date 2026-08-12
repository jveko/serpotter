use crate::{ProviderError, ProviderResult, ProviderSearchParams, SVC_XAI};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use serpotter_core::SearchItem;

/// xAI `web_search` caps `allowed_domains` / `excluded_domains` at 5 entries each
/// (docs.x.ai/developers/tools/web-search). Fail loudly rather than truncating.
const MAX_DOMAIN_FILTERS: usize = 5;

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
            model: std::env::var("XAI_MODEL").unwrap_or_else(|_| "grok-4.5".into()),
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
        let wants_web = p
            .sources
            .map(|s| s.iter().any(|x| x == "web"))
            .unwrap_or(false);

        // Refuse shapes this dialect cannot express honestly: no page content
        // on either path, no domain filter on the social (X) path, and never a
        // silent web+x mix (the social branch would drop the web intent).
        validate_xai_search_policy(
            wants_x,
            wants_web,
            p.include_content,
            p.include_domains,
            p.exclude_domains,
        )?;
        // The web_search tool has no structured date param; warn once per
        // request when NL prose is the only carrier.
        if let Some(reason) = criteria_may_be_best_effort(p.from_date, p.to_date, p.time_range) {
            tracing::warn!(provider = SVC_XAI, "{reason}");
        }

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
                p.time_range,
            );
            (json!([]), prompt)
        } else {
            if let Some(d) = p.include_domains {
                if d.len() > MAX_DOMAIN_FILTERS {
                    return Err(ProviderError::Unsupported {
                        provider: SVC_XAI.into(),
                        action: "search",
                        detail: format!(
                            "allowed_domains supports at most {MAX_DOMAIN_FILTERS} entries, got {}",
                            d.len()
                        ),
                    });
                }
            }
            if let Some(d) = p.exclude_domains {
                if d.len() > MAX_DOMAIN_FILTERS {
                    return Err(ProviderError::Unsupported {
                        provider: SVC_XAI.into(),
                        action: "search",
                        detail: format!(
                            "excluded_domains supports at most {MAX_DOMAIN_FILTERS} entries, got {}",
                            d.len()
                        ),
                    });
                }
            }
            let mut tool = json!({ "type": "web_search" });
            if let Some(d) = p.include_domains {
                if !d.is_empty() {
                    tool["allowed_domains"] = json!(d);
                }
            }
            if let Some(d) = p.exclude_domains {
                if !d.is_empty() {
                    tool["excluded_domains"] = json!(d);
                }
            }
            let prompt = append_xai_prompt_constraints(
                p.query,
                false,
                None,
                None,
                p.from_date,
                p.to_date,
                p.time_range,
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

        let up: Up = res.json().await?;
        let answer = extract_output_text(&up);

        // B2/B22: honest token capture from /responses `usage`. Only top-level
        // numeric counters are mapped; `total_tokens` falls back to the
        // input+output sum when the wire omits it.
        let input_tokens = up
            .usage
            .as_ref()
            .and_then(|u| usage_tokens(u.input_tokens.as_ref()));
        let output_tokens = up
            .usage
            .as_ref()
            .and_then(|u| usage_tokens(u.output_tokens.as_ref()));
        let total_tokens = up
            .usage
            .as_ref()
            .and_then(|u| usage_tokens(u.total_tokens.as_ref()))
            .or_else(|| match (input_tokens, output_tokens) {
                (Some(a), Some(b)) => Some(a + b),
                _ => None,
            });

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
                    snippet: None,
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
                                            snippet: None,
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
            input_tokens,
            output_tokens,
            total_tokens,
            // No per-call cost model for xAI this wave (B2 captures tokens only).
            cost: None,
        })
    }

    /// B19: one-shot text completion for the deep-research synthesis loop —
    /// posts to `{base}/responses` with the SAME dialect rules as [`search`]
    /// (Bearer, direct client, no tools, no `x_search`) and returns the
    /// `output_text` answer (or the concatenated output parts when
    /// `output_text` is absent). `model` overrides the env default when
    /// `Some`; `max_tokens` bounds `max_output_tokens`.
    pub async fn complete(
        &self,
        api_key: &str,
        system: &str,
        user: &str,
        model: Option<&str>,
        max_tokens: u32,
    ) -> Result<String, ProviderError> {
        let url = format!("{}/responses", self.base_url);
        let model = model.unwrap_or(&self.model);
        let body = json!({
            "model": model,
            "input": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "max_output_tokens": max_tokens,
            "store": false,
        });
        let res = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {api_key}"))
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
        let up: Complete = res.json().await?;
        Ok(extract_complete_text(&up).unwrap_or_default())
    }
}

/// /responses wire shape shared by [`search`] and [`complete`] (module scope
/// so both methods and the parser tests reuse the exact same fields).
#[derive(Deserialize)]
pub(crate) struct Up {
    pub(crate) output_text: Option<String>,
    pub(crate) citations: Option<Vec<Cit>>,
    pub(crate) output: Option<Vec<OutMsg>>,
    pub(crate) usage: Option<Usage>,
}
#[derive(Deserialize)]
pub(crate) struct Usage {
    /// Token counters. Usually plain integers; some endpoints nest them
    /// (`input_tokens: {"tokens": N}` for reasoning detail) — we only map
    /// top-level numeric fields and leave nested shapes as None (honest:
    /// unknown shape, never a fabricated number).
    pub(crate) input_tokens: Option<serde_json::Value>,
    pub(crate) output_tokens: Option<serde_json::Value>,
    pub(crate) total_tokens: Option<serde_json::Value>,
}

/// Map a /responses `usage` token field to `u64`: top-level number only.
fn usage_tokens(v: Option<&serde_json::Value>) -> Option<u64> {
    v.and_then(|x| x.as_u64())
}
#[derive(Deserialize)]
pub(crate) struct Cit {
    pub(crate) title: Option<String>,
    pub(crate) url: Option<String>,
}
#[derive(Deserialize)]
pub(crate) struct OutMsg {
    pub(crate) content: Option<Vec<Part>>,
}
#[derive(Deserialize)]
pub(crate) struct Part {
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub(crate) kind: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) annotations: Option<Vec<Ann>>,
}
#[derive(Deserialize)]
pub(crate) struct Ann {
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) url: Option<String>,
}

/// Shared /responses answer extraction (B19): `output_text` first, then the
/// concatenated `output[].content[].text` parts. `None` when the wire carried
/// neither — callers must never fabricate an answer from an empty body.
pub(crate) fn extract_output_text(up: &Up) -> Option<String> {
    if let Some(t) = up.output_text.as_deref() {
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let mut buf = String::new();
    if let Some(msgs) = &up.output {
        for m in msgs {
            if let Some(parts) = &m.content {
                for part in parts {
                    if let Some(t) = &part.text {
                        buf.push_str(t);
                    }
                }
            }
        }
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// /responses completion wire shape for [`XaiClient::complete`] (B19) — the
/// same `output_text`/`output[].content[].text` dialect, no citations.
#[derive(Deserialize)]
pub(crate) struct Complete {
    pub(crate) output_text: Option<String>,
    pub(crate) output: Option<Vec<CompleteOutMsg>>,
}
#[derive(Deserialize)]
pub(crate) struct CompleteOutMsg {
    pub(crate) content: Option<Vec<CompletePart>>,
}
#[derive(Deserialize)]
pub(crate) struct CompletePart {
    pub(crate) text: Option<String>,
}

/// Lightweight twin of [`extract_output_text`] for the `Complete` shape
/// (no citations/annotations on the completion wire).
pub(crate) fn extract_complete_text(up: &Complete) -> Option<String> {
    if let Some(t) = up.output_text.as_deref() {
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let mut buf = String::new();
    if let Some(msgs) = &up.output {
        for m in msgs {
            if let Some(parts) = &m.content {
                for part in parts {
                    if let Some(t) = &part.text {
                        buf.push_str(t);
                    }
                }
            }
        }
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Refuse request shapes the xAI dialect cannot express honestly, instead of
/// silently dropping user intent. Pure — no network — so unit tests can pin
/// the wire policy without an HTTP server.
///
/// - `include_content` is refused on both paths: xAI `web_search` results
///   carry title+url only, never page content, and we will not fabricate it.
/// - On the social (X) path `allowed_domains`/`excluded_domains` have no
///   structured field (the tool list is empty), so any non-empty filter is
///   refused rather than truncated.
/// - A mixed `sources=["web","x"]` request is refused: this client serves
///   exactly one dialect per call, and the social branch would silently drop
///   the web intent. Callers wanting both legs must use hybrid routing.
pub(crate) fn validate_xai_search_policy(
    wants_x: bool,
    wants_web: bool,
    include_content: bool,
    include_domains: Option<&[String]>,
    exclude_domains: Option<&[String]>,
) -> Result<(), ProviderError> {
    if wants_x && wants_web {
        return Err(ProviderError::Unsupported {
            provider: SVC_XAI.into(),
            action: "search",
            detail: "xai provider cannot serve web sources; use hybrid or omit sources".into(),
        });
    }
    if include_content {
        return Err(ProviderError::Unsupported {
            provider: SVC_XAI.into(),
            action: "search",
            detail: "xAI web_search results carry no page content; set include_content=false or use a content-capable provider"
                .into(),
        });
    }
    if wants_x
        && (include_domains.is_some_and(|d| !d.is_empty())
            || exclude_domains.is_some_and(|d| !d.is_empty()))
    {
        return Err(ProviderError::Unsupported {
            provider: SVC_XAI.into(),
            action: "search",
            detail: "social/X search cannot express allowed_domains/excluded_domains".into(),
        });
    }
    Ok(())
}

/// Best-effort marker for date/time-range criteria: present exactly when the
/// prompt builder will actually carry the constraint as NL prose (dates by
/// presence, time_range when non-blank). The xAI `web_search` dialect has no
/// structured date parameter, so `search()` logs this once per request.
pub(crate) fn criteria_may_be_best_effort(
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
) -> Option<&'static str> {
    let any_date = from_date.is_some() || to_date.is_some();
    let any_range = time_range.is_some_and(|t| !t.trim().is_empty());
    (any_date || any_range).then_some(
        "from_date/to_date/time_range are best-effort NL guidance: the xAI web_search dialect has no structured date parameter",
    )
}

/// Append handle/date/time constraints to an xAI user prompt.
///
/// Dialect note: the xAI `web_search` tool carries only `type` plus
/// `allowed_domains`/`excluded_domains` — there is NO structured date or
/// time-range parameter. `from_date`/`to_date`/`time_range` are therefore
/// conveyed only as best-effort NL prose (`search()` logs a one-time warn
/// when they are set). Handles only apply on the social (X) path; dates and
/// time_range apply to both.
pub(crate) fn append_xai_prompt_constraints(
    base: &str,
    social: bool,
    allowed_x_handles: Option<&[String]>,
    excluded_x_handles: Option<&[String]>,
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
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
        (None, None) => {
            if let Some(tr) = time_range {
                let t = tr.trim();
                if !t.is_empty() {
                    parts.push(format!("time range: {t}"));
                }
            }
        }
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

    // ---- pure policy helpers (no network) ----

    #[test]
    fn policy_rejects_include_content_on_web() {
        let err = validate_xai_search_policy(false, false, true, None, None)
            .expect_err("include_content must be refused on the web path");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, SVC_XAI);
                assert_eq!(action, "search");
                assert!(detail.contains("include_content"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn policy_rejects_include_content_on_social_too() {
        let err = validate_xai_search_policy(true, false, true, None, None)
            .expect_err("include_content must be refused on the social path too");
        assert!(matches!(err, ProviderError::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn policy_rejects_social_include_domains() {
        let domains = vec!["a.example".into()];
        let err = validate_xai_search_policy(true, false, false, Some(&domains), None)
            .expect_err("social + include_domains must be refused");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, SVC_XAI);
                assert_eq!(action, "search");
                assert!(detail.contains("allowed_domains"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn policy_rejects_social_exclude_domains() {
        let domains = vec!["b.example".into()];
        let err = validate_xai_search_policy(true, false, false, None, Some(&domains))
            .expect_err("social + exclude_domains must be refused");
        match err {
            ProviderError::Unsupported { detail, .. } => {
                assert!(detail.contains("excluded_domains"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn policy_allows_web_domains_and_social_without_domains() {
        let domains = vec!["a.example".into()];
        validate_xai_search_policy(false, false, false, Some(&domains), None)
            .expect("web include_domains must stay allowed");
        validate_xai_search_policy(true, false, false, None, None)
            .expect("social without domains must stay allowed");
        validate_xai_search_policy(false, false, false, None, None)
            .expect("plain web without constraints must stay allowed");
    }

    #[test]
    fn policy_rejects_mixed_web_and_x_sources() {
        let err = validate_xai_search_policy(true, true, false, None, None)
            .expect_err("wants_x + wants_web must be refused loudly");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, SVC_XAI);
                assert_eq!(action, "search");
                assert!(detail.contains("cannot serve web sources"), "{detail}");
                assert!(detail.contains("hybrid"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn mixed_web_x_sources_refused_loudly_before_network() {
        let client = XaiClient::new("http://127.0.0.1:9");
        let mixed = vec!["web".to_string(), "x".to_string()];
        let mut p = params(None, None);
        p.sources = Some(mixed.as_slice());
        let err = client
            .search(p)
            .await
            .expect_err("web+x on the xai provider must refuse before any network call");
        match err {
            ProviderError::Unsupported { detail, .. } => {
                assert!(detail.contains("cannot serve web sources"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn x_only_sources_stay_social() {
        let client = XaiClient::new("http://127.0.0.1:9");
        let x = vec!["x".to_string()];
        let mut p = params(None, None);
        p.sources = Some(x.as_slice());
        // Not refused locally: the request proceeds to the (unreachable) upstream.
        let err = client.search(p).await.expect_err("connect to :9");
        assert!(
            !matches!(err, ProviderError::Unsupported { .. }),
            "x-only sources must not be refused, got {err:?}"
        );
    }

    #[tokio::test]
    async fn web_only_sources_stay_web() {
        let client = XaiClient::new("http://127.0.0.1:9");
        let web = vec!["web".to_string()];
        let mut p = params(None, None);
        p.sources = Some(web.as_slice());
        let err = client.search(p).await.expect_err("connect to :9");
        assert!(
            !matches!(err, ProviderError::Unsupported { .. }),
            "web-only sources must not be refused, got {err:?}"
        );
    }

    #[test]
    fn date_criteria_mark_best_effort_without_error() {
        assert!(
            criteria_may_be_best_effort(Some("2026-01-01"), None, None).is_some(),
            "from_date marks best-effort"
        );
        assert!(
            criteria_may_be_best_effort(None, Some("2026-01-31"), None).is_some(),
            "to_date marks best-effort"
        );
        assert!(
            criteria_may_be_best_effort(None, None, Some("week")).is_some(),
            "time_range marks best-effort"
        );
        assert!(
            criteria_may_be_best_effort(None, None, None).is_none(),
            "no criteria -> no marker"
        );
        assert!(
            criteria_may_be_best_effort(None, None, Some("  ")).is_none(),
            "blank time_range is not a real constraint"
        );
    }

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
            None,
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
            None,
        );
        assert!(!out.contains("@someone"), "{out}");
        assert!(out.contains("from 2025-06-01"), "{out}");
        assert!(out.starts_with("quantum computing"), "{out}");
    }

    #[test]
    fn time_range_used_when_no_abs_dates() {
        let out =
            append_xai_prompt_constraints("posts", true, None, None, None, None, Some("week"));
        assert!(out.contains("time range: week"), "{out}");
    }

    #[test]
    fn abs_dates_prefer_over_time_range() {
        let out = append_xai_prompt_constraints(
            "posts",
            true,
            None,
            None,
            Some("2026-01-01"),
            None,
            Some("week"),
        );
        assert!(out.contains("from 2026-01-01"), "{out}");
        assert!(!out.contains("time range"), "{out}");
    }

    #[test]
    fn no_constraints_returns_base() {
        let out = append_xai_prompt_constraints("plain", true, None, None, None, None, None);
        assert_eq!(out, "plain");
    }

    fn params<'a>(
        include_domains: Option<&'a [String]>,
        exclude_domains: Option<&'a [String]>,
    ) -> ProviderSearchParams<'a> {
        ProviderSearchParams {
            query: "q",
            max_results: 1,
            api_key: "k",
            include_content: false,
            include_answer: false,
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: None,
            sources: None,
            include_domains,
            exclude_domains,
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: None,
            to_date: None,
            time_range: None,
            country: None,
            exact_match: None,
        }
    }

    #[tokio::test]
    async fn over_cap_domains_error_not_truncate() {
        let client = XaiClient::new("http://127.0.0.1:9");
        let domains: Vec<String> = (1..=6).map(|i| format!("d{i}.example")).collect();
        // 6 > 5: must fail validation before any network call, never silently drop.
        let err = client
            .search(params(Some(&domains), None))
            .await
            .expect_err("6 domains must fail validation before network");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, SVC_XAI);
                assert_eq!(action, "search");
                assert!(detail.contains("allowed_domains"), "{detail}");
                assert!(detail.contains("5"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn excluded_domains_over_cap_errors() {
        let client = XaiClient::new("http://127.0.0.1:9");
        let domains: Vec<String> = (1..=6).map(|i| format!("x{i}.example")).collect();
        let err = client
            .search(params(None, Some(&domains)))
            .await
            .expect_err("6 excluded domains must fail validation");
        match err {
            ProviderError::Unsupported { detail, .. } => {
                assert!(detail.contains("excluded_domains"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn at_cap_domains_pass_validation() {
        let client = XaiClient::new("http://127.0.0.1:9");
        let domains: Vec<String> = (1..=5).map(|i| format!("d{i}.example")).collect();
        // 5 is the documented cap: local validation passes and the request
        // proceeds to the (unreachable) upstream — a connection error, not
        // a local Unsupported error.
        let err = client
            .search(params(Some(&domains), None))
            .await
            .expect_err("connect to :9");
        assert!(
            !matches!(err, ProviderError::Unsupported { .. }),
            "at-cap domains must not error locally, got {err:?}"
        );
    }

    /// Serve one canned HTTP response; returns the base URL to point the client at.
    fn spawn_responses_server(body: String) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let len = body.len();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}"
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn citations_without_snippets_are_none() {
        // xAI citations carry title+url only — no snippet payload. The honest
        // serialization is snippet: None, not Some("").
        let body = r#"{
            "output_text": "answer text",
            "citations": [
                { "title": "T1", "url": "https://a.example" },
                { "title": "T2", "url": "https://b.example" }
            ],
            "output": [
                { "content": [
                    {
                        "type": "output_text",
                        "text": "x",
                        "annotations": [
                            { "type": "url_citation", "title": "A1", "url": "https://c.example" }
                        ]
                    }
                ] }
            ]
        }"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client.search(params(None, None)).await.expect("search");
        assert_eq!(out.items.len(), 3, "2 citations + 1 url_citation");
        assert_eq!(out.answer.as_deref(), Some("answer text"));
        for item in &out.items {
            assert!(
                item.snippet.is_none(),
                "snippet must be None, got {:?}",
                item.snippet
            );
        }
    }

    // ---- B2/B22: /responses usage token capture + B8 default model ----

    /// Serve one canned response and capture the request body that arrived.
    fn spawn_capture_server(body: String) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            let text = String::from_utf8_lossy(&buf);
                            if let Some((head, recv_body)) = text.split_once("\r\n\r\n") {
                                let declared = head
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        (k.trim().eq_ignore_ascii_case("content-length"))
                                            .then(|| v.trim().parse::<usize>().ok())
                                            .flatten()
                                    })
                                    .unwrap_or(0);
                                if recv_body.len() >= declared {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let raw = String::from_utf8_lossy(&buf).to_string();
                let (_head, body_part) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
                let _ = tx.send(body_part.to_string());
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}"), rx)
    }

    #[tokio::test]
    async fn search_parses_usage_tokens() {
        // Plain top-level counters: input/output parsed, total falls back to
        // the input+output sum when the wire omits it.
        let body = r#"{
            "output_text": "a",
            "usage": { "input_tokens": 10, "output_tokens": 20 },
            "citations": []
        }"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client.search(params(None, None)).await.expect("search");
        assert_eq!(out.input_tokens, Some(10));
        assert_eq!(out.output_tokens, Some(20));
        assert_eq!(out.total_tokens, Some(30), "sum fallback");
        assert!(out.cost.is_none(), "xAI has no per-call cost model");
    }

    #[tokio::test]
    async fn search_usage_total_tokens_wins_over_sum() {
        // When the wire reports total_tokens it must win over the computed sum.
        let body = r#"{
            "output_text": "a",
            "usage": { "input_tokens": 10, "output_tokens": 20, "total_tokens": 50 },
            "citations": []
        }"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client.search(params(None, None)).await.expect("search");
        assert_eq!(out.input_tokens, Some(10));
        assert_eq!(out.output_tokens, Some(20));
        assert_eq!(out.total_tokens, Some(50), "wire total wins");
    }

    #[tokio::test]
    async fn search_nested_usage_counters_stay_none() {
        // Nested counters (e.g. reasoning detail) are NOT dug into — the
        // honest value for an unknown shape is None, never a fabricated token.
        let body = r#"{
            "output_text": "a",
            "usage": { "input_tokens": { "tokens": 5 }, "output_tokens": 5 },
            "citations": []
        }"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client.search(params(None, None)).await.expect("search");
        assert_eq!(out.input_tokens, None, "nested input_tokens is not numeric");
        assert_eq!(out.output_tokens, Some(5));
        assert_eq!(
            out.total_tokens, None,
            "no total and no numeric input -> None"
        );
    }

    #[tokio::test]
    async fn search_without_usage_leaves_usage_none() {
        let body = r#"{"output_text": "a", "citations": []}"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client.search(params(None, None)).await.expect("search");
        assert!(out.input_tokens.is_none());
        assert!(out.output_tokens.is_none());
        assert!(out.total_tokens.is_none());
    }

    #[tokio::test]
    async fn default_model_is_grok_4_5_and_env_override_wins() {
        // B8: compiled default bumped 4.3 → 4.5; the XAI_MODEL env override is
        // unchanged and must beat the default. Both assertions live in ONE test
        // (no other test touches XAI_MODEL) so parallel execution cannot race
        // the process-global env var.
        std::env::remove_var("XAI_MODEL");
        let (base, rx) = spawn_capture_server(r#"{"output_text":"a","citations":[]}"#.into());
        let client = XaiClient::new(base);
        let _out = client.search(params(None, None)).await.expect("search");
        let body = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request captured");
        let v: serde_json::Value = serde_json::from_str(&body).expect("request body is JSON");
        assert_eq!(v["model"], "grok-4.5", "default model bumped to grok-4.5");

        std::env::set_var("XAI_MODEL", "grok-custom");
        let (base, rx) = spawn_capture_server(r#"{"output_text":"a","citations":[]}"#.into());
        let client = XaiClient::new(base);
        let _out = client.search(params(None, None)).await.expect("search");
        let body = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request captured");
        let v: serde_json::Value = serde_json::from_str(&body).expect("request body is JSON");
        assert_eq!(v["model"], "grok-custom", "env override must win");

        std::env::remove_var("XAI_MODEL");
    }

    // ---- B19: complete() synthesis dialect + parser (canned /responses) ----

    #[tokio::test]
    async fn complete_parses_output_text() {
        let body = r#"{"output_text":"synthesized answer","output":[{"content":[{"type":"output_text","text":"ignored"}]}]}"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client
            .complete("k", "system prose", "user prose", None, 1200)
            .await
            .expect("complete against canned server");
        assert_eq!(out, "synthesized answer", "output_text wins");
    }

    #[tokio::test]
    async fn complete_falls_back_to_output_parts() {
        // No output_text on the wire: the parser concatenates output parts.
        let body = r#"{"output":[{"content":[{"type":"output_text","text":"part one "},{"type":"output_text","text":"part two"}]}]}"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client
            .complete("k", "s", "u", Some("grok-test"), 500)
            .await
            .expect("complete");
        assert_eq!(out, "part one part two");
    }

    #[tokio::test]
    async fn complete_empty_wire_returns_empty_not_fabricated() {
        // No answer payload: an empty string, never a fabricated answer — the
        // deep loop treats "" as "synthesis unavailable".
        let body = r#"{"id":"x","model":"grok"}"#;
        let base = spawn_responses_server(body.to_string());
        let client = XaiClient::new(base);
        let out = client
            .complete("k", "s", "u", None, 100)
            .await
            .expect("complete");
        assert!(out.is_empty(), "empty wire -> empty answer: {out:?}");
    }

    #[tokio::test]
    async fn complete_upstream_error_is_honest() {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 11\r\nconnection: close\r\n\r\nrate limited";
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let client = XaiClient::new(format!("http://{addr}"));
        let err = client
            .complete("k", "s", "u", None, 100)
            .await
            .expect_err("429 must surface as an upstream error");
        match err {
            ProviderError::Upstream {
                provider, status, ..
            } => {
                assert_eq!(provider, "xai");
                assert_eq!(status, 429);
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }
}
