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

    // --- F47: request-side wire format (path, headers, body field names) -----

    /// Request captured by the loopback recording server.
    struct RecordedRequest {
        request_line: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl RecordedRequest {
        fn path(&self) -> &str {
            self.request_line.split_whitespace().nth(1).unwrap_or("")
        }

        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }

        fn body_json(&self) -> serde_json::Value {
            serde_json::from_str(&self.body).expect("request body is JSON")
        }
    }

    /// Serve one canned JSON response and capture the request that arrived
    /// (std::thread TcpListener pattern, extended to record wire bytes).
    fn spawn_recording_server(
        response: serde_json::Value,
    ) -> (String, std::sync::mpsc::Receiver<RecordedRequest>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let body = serde_json::to_string(&response).expect("serialize canned response");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain until the declared Content-Length is satisfied (a single
                // read can return a partial request on loopback).
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
                let (head, body_part) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("").to_string();
                let headers = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    .collect();
                let _ = tx.send(RecordedRequest {
                    request_line,
                    headers,
                    body: body_part.to_string(),
                });
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

    /// Firecrawl v2 search authenticates via Bearer header and uses the
    /// camelCase v2 body keys (sources/categories/includeDomains/tbs).
    #[tokio::test]
    async fn search_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "data": { "web": [{ "title": "T", "url": "https://t.example", "description": "d" }] }
        }));
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let include = vec!["docs.rs".to_string()];
        let p = ProviderSearchParams {
            query: "rust wire",
            max_results: 5,
            api_key: "fc-secret-key",
            include_content: true,
            include_answer: false,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: Some(&["news".to_string()]),
            sources: Some(&["web".to_string(), "news".to_string()]),
            include_domains: Some(&include),
            exclude_domains: None,
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: Some("2026-02-01"),
            to_date: Some("2026-02-28"),
            time_range: Some("week"),
            country: None,
            exact_match: None,
        };
        let out = client.search(&http, p).await.expect("search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/v2/search", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer fc-secret-key",
            "firecrawl auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["query"], "rust wire");
        assert_eq!(b["limit"], 5);
        assert_eq!(b["sources"], serde_json::json!(["web", "news"]));
        assert_eq!(b["categories"], serde_json::json!(["news"]));
        assert_eq!(b["includeDomains"], serde_json::json!(["docs.rs"]));
        // Absolute dates → tbs cdr range (US M/D/YYYY).
        assert_eq!(b["tbs"], "cdr:1,cd_min:2/1/2026,cd_max:2/28/2026");
        // include_content → scrapeOptions markdown main-content.
        assert_eq!(
            b["scrapeOptions"]["formats"],
            serde_json::json!(["markdown"])
        );
        assert_eq!(b["scrapeOptions"]["onlyMainContent"], true);
        // Response parses back from the wire.
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].title, "T");
        assert_eq!(out.items[0].snippet.as_deref(), Some("d"));
        assert_eq!(out.items[0].provider.as_deref(), Some("firecrawl"));
    }

    /// Firecrawl extract is POST /v1/scrape with Bearer + url/formats body.
    #[tokio::test]
    async fn extract_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "data": {
                "markdown": "# md",
                "metadata": { "title": "Page", "sourceURL": "https://example.com/page" }
            }
        }));
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .extract(&http, "https://example.com/page", "fc-extract-key")
            .await
            .expect("extract against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/v1/scrape", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer fc-extract-key",
            "firecrawl extract auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["url"], "https://example.com/page");
        assert_eq!(b["formats"], serde_json::json!(["markdown"]));
        assert_eq!(b["onlyMainContent"], true);
        assert_eq!(out.content, "# md");
        assert_eq!(out.title.as_deref(), Some("Page"));
        assert_eq!(
            out.url, "https://example.com/page",
            "sourceURL wins over input"
        );
        assert_eq!(out.provider, "firecrawl");
    }
}
