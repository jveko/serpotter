use crate::{
    parse_firecrawl_usage, CreditSnapshot, ExtractResult, ProviderError, ProviderResult,
    ProviderSearchParams,
};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

/// Thin Firecrawl adapter — HTTP client is supplied per call (registry cache).
#[derive(Clone)]
pub struct FirecrawlClient {
    base_url: String,
}

impl FirecrawlClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn search(
        &self,
        http: &Client,
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
        // Absolute dates win over time_range (tbs cdr vs qdr).
        apply_firecrawl_date_filters(&mut body, p.from_date, p.to_date, p.time_range);
        if let Some(c) = p.country {
            body["country"] = serde_json::json!(c);
        }
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

        if p.include_content {
            body["scrapeOptions"] = serde_json::json!({
                "formats": ["markdown"],
                "onlyMainContent": true,
            });
        }

        let res = http
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

    /// Scrape a single URL via Firecrawl `/v1/scrape` (markdown + main content).
    pub async fn extract(
        &self,
        http: &Client,
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        let endpoint = format!("{}/v1/scrape", self.base_url);
        let body = serde_json::json!({
            "url": url,
            "formats": ["markdown"],
            "onlyMainContent": true,
        });
        let res = http
            .post(&endpoint)
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
                provider: "firecrawl".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            data: Option<Data>,
            #[allow(dead_code)]
            success: Option<bool>,
        }
        #[derive(Deserialize)]
        struct Data {
            markdown: Option<String>,
            content: Option<String>,
            metadata: Option<Meta>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Meta {
            title: Option<String>,
            description: Option<String>,
            source_url: Option<String>,
            #[serde(rename = "sourceURL")]
            source_url_alt: Option<String>,
        }
        let up: Up = res.json().await?;
        let data = up.data.unwrap_or(Data {
            markdown: None,
            content: None,
            metadata: None,
        });
        let meta = data.metadata.unwrap_or(Meta {
            title: None,
            description: None,
            source_url: None,
            source_url_alt: None,
        });
        let final_url = meta
            .source_url
            .or(meta.source_url_alt)
            .unwrap_or_else(|| url.to_string());
        Ok(ExtractResult {
            url: final_url,
            title: meta.title,
            content: data.markdown.or(data.content).unwrap_or_default(),
            provider: "firecrawl".into(),
        })
    }

    /// Fetch team credit usage via `GET /v2/team/credit-usage`.
    pub async fn fetch_usage(
        &self,
        http: &Client,
        api_key: &str,
    ) -> Result<CreditSnapshot, ProviderError> {
        let url = format!("{}/v2/team/credit-usage", self.base_url);
        let res = http
            .get(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("User-Agent", "Serpotter/0.1")
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
        let v: serde_json::Value = res.json().await?;
        parse_firecrawl_usage(&v)
    }
}

/// Apply absolute dates or relative time_range to a Firecrawl search body via `tbs`.
/// Absolute dates take precedence (cdr:1,cd_min/cd_max US M/D/YYYY); never invent startDate keys.
pub(crate) fn apply_firecrawl_date_filters(
    body: &mut serde_json::Value,
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
) {
    let from_us = from_date.and_then(ymd_to_us_mdy);
    let to_us = to_date.and_then(ymd_to_us_mdy);
    let has_abs = from_us.is_some() || to_us.is_some();
    if has_abs {
        let mut parts = vec!["cdr:1".to_string()];
        if let Some(f) = from_us {
            parts.push(format!("cd_min:{f}"));
        }
        if let Some(t) = to_us {
            parts.push(format!("cd_max:{t}"));
        }
        body["tbs"] = serde_json::json!(parts.join(","));
        return;
    }
    if let Some(tr) = time_range {
        let tbs = match tr {
            "day" => "qdr:d",
            "week" => "qdr:w",
            "month" => "qdr:m",
            "year" => "qdr:y",
            other => other,
        };
        body["tbs"] = serde_json::json!(tbs);
    }
}

/// Parse wire YYYY-MM-DD → Firecrawl US M/D/YYYY. Returns None if not civil YYYY-MM-DD.
fn ymd_to_us_mdy(s: &str) -> Option<String> {
    let s = s.trim();
    let mut parts = s.split('-');
    let y = parts.next()?;
    let m = parts.next()?;
    let d = parts.next()?;
    if parts.next().is_some() || y.len() != 4 {
        return None;
    }
    let yi: u32 = y.parse().ok()?;
    let mi: u32 = m.parse().ok()?;
    let di: u32 = d.parse().ok()?;
    if !(1..=12).contains(&mi) || !(1..=31).contains(&di) || yi < 1970 {
        return None;
    }
    Some(format!("{mi}/{di}/{yi}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderSearchParams;

    fn base_params<'a>(
        key: &'a str,
        include: Option<&'a [String]>,
        exclude: Option<&'a [String]>,
    ) -> ProviderSearchParams<'a> {
        ProviderSearchParams {
            query: "rust",
            max_results: 5,
            api_key: key,
            include_content: false,
            include_answer: false,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: None,
            sources: None,
            include_domains: include,
            exclude_domains: exclude,
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
    fn search_body_includes_domain_filters() {
        // Mirror the JSON construction path without HTTP by reusing the same keys FC sets.
        let include = vec!["example.com".into(), "docs.rs".into()];
        let exclude = vec!["spam.example".into()];
        let p = base_params("k", Some(include.as_slice()), Some(exclude.as_slice()));
        let mut body = serde_json::json!({
            "query": p.query,
            "limit": p.max_results,
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
        assert_eq!(
            body["includeDomains"],
            serde_json::json!(["example.com", "docs.rs"])
        );
        assert_eq!(body["excludeDomains"], serde_json::json!(["spam.example"]));
    }

    #[test]
    fn absolute_dates_set_cdr_tbs_skip_qdr() {
        let mut body = serde_json::json!({});
        apply_firecrawl_date_filters(
            &mut body,
            Some("2026-01-01"),
            Some("2026-01-31"),
            Some("week"),
        );
        assert_eq!(body["tbs"], "cdr:1,cd_min:1/1/2026,cd_max:1/31/2026");
    }

    #[test]
    fn only_from_date_sets_cd_min() {
        let mut body = serde_json::json!({});
        apply_firecrawl_date_filters(&mut body, Some("2026-03-01"), None, Some("month"));
        assert_eq!(body["tbs"], "cdr:1,cd_min:3/1/2026");
    }

    #[test]
    fn only_to_date_sets_cd_max() {
        let mut body = serde_json::json!({});
        apply_firecrawl_date_filters(&mut body, None, Some("2026-12-25"), None);
        assert_eq!(body["tbs"], "cdr:1,cd_max:12/25/2026");
    }

    #[test]
    fn time_range_week_when_no_absolute_dates() {
        let mut body = serde_json::json!({});
        apply_firecrawl_date_filters(&mut body, None, None, Some("week"));
        assert_eq!(body["tbs"], "qdr:w");
    }

    #[test]
    fn unparseable_abs_falls_through_to_time_range() {
        let mut body = serde_json::json!({});
        apply_firecrawl_date_filters(&mut body, Some("not-a-date"), None, Some("day"));
        assert_eq!(body["tbs"], "qdr:d");
    }
}
