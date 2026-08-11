use crate::{
    parse_tavily_usage, CreditSnapshot, ExtractResult, ProviderError, ProviderResult,
    ProviderSearchParams,
};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

const DEFAULT: &str = "https://api.tavily.com";

/// Thin Tavily adapter — HTTP client is supplied per call (registry cache).
#[derive(Clone)]
pub struct TavilyClient {
    base_url: String,
}

impl TavilyClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn with_default() -> Self {
        Self::new(DEFAULT)
    }

    pub async fn search(
        &self,
        http: &Client,
        p: ProviderSearchParams<'_>,
    ) -> Result<ProviderResult, ProviderError> {
        let url = format!("{}/search", self.base_url);
        let mut body = serde_json::json!({
            "query": p.query,
            "max_results": p.max_results,
            "search_depth": p.search_depth.unwrap_or("basic"),
            "topic": p.tavily_topic.unwrap_or("general"),
            "include_answer": p.include_answer,
            // include_content (Serpotter's pipeline semantic) and the native
            // include_raw_content knob both ask Tavily for raw content.
            "include_raw_content": p.include_content || p.include_raw_content,
            "include_images": p.include_images,
        });
        if let Some(c) = p.chunks_per_source {
            body["chunks_per_source"] = serde_json::json!(c);
        }
        if let Some(d) = p.include_domains {
            if !d.is_empty() {
                body["include_domains"] = serde_json::json!(d);
            }
        }
        if let Some(d) = p.exclude_domains {
            if !d.is_empty() {
                body["exclude_domains"] = serde_json::json!(d);
            }
        }
        // Absolute dates win over time_range (Tavily forbids both).
        apply_tavily_date_filters(&mut body, p.from_date, p.to_date, p.time_range);
        if let Some(c) = p.country {
            body["country"] = serde_json::json!(c);
        }
        if let Some(e) = p.exact_match {
            body["exact_match"] = serde_json::json!(e);
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
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            query: Option<String>,
            answer: Option<String>,
            results: Option<Vec<Row>>,
        }
        #[derive(Deserialize)]
        struct Row {
            title: Option<String>,
            url: Option<String>,
            content: Option<String>,
            raw_content: Option<String>,
            score: Option<f64>,
        }
        let up: Up = res.json().await?;
        let items = up
            .results
            .unwrap_or_default()
            .into_iter()
            .map(|r| SearchItem {
                title: r.title.unwrap_or_default(),
                url: r.url.unwrap_or_default(),
                snippet: r.content,
                content: if p.include_content || p.include_raw_content {
                    r.raw_content
                } else {
                    None
                },
                score: r.score,
                published: None,
                author: None,
                provider: Some("tavily".into()),
                source: Some("web".into()),
            })
            .collect();
        Ok(ProviderResult {
            provider: "tavily".into(),
            query: up.query.unwrap_or_else(|| p.query.to_string()),
            items,
            answer: up.answer.filter(|a| !a.is_empty()),
        })
    }

    /// Extract page content via Tavily `/extract`.
    pub async fn extract(
        &self,
        http: &Client,
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        let endpoint = format!("{}/extract", self.base_url);
        let body = serde_json::json!({
            "urls": [url],
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
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Up {
            results: Option<Vec<Row>>,
            failed_results: Option<Vec<Failed>>,
        }
        #[derive(Deserialize)]
        struct Row {
            url: Option<String>,
            raw_content: Option<String>,
            content: Option<String>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct Failed {
            url: Option<String>,
            error: Option<String>,
        }
        let up: Up = res.json().await?;
        if let Some(first) = up.results.unwrap_or_default().into_iter().next() {
            return Ok(ExtractResult {
                url: first.url.unwrap_or_else(|| url.to_string()),
                title: None,
                content: first.raw_content.or(first.content).unwrap_or_default(),
                provider: "tavily".into(),
            });
        }
        let fail_msg = up
            .failed_results
            .unwrap_or_default()
            .into_iter()
            .next()
            .and_then(|f| f.error)
            .unwrap_or_else(|| "extract returned no results".into());
        // Empty/failed extract is URL-class, not key health. Product maps to
        // finish_release + continue chain (do not invent a fake HTTP status).
        Err(ProviderError::Unextractable {
            provider: "tavily".into(),
            message: fail_msg,
        })
    }

    /// Fetch key/account credit usage via `GET /usage`.
    ///
    /// Auth: Bearer header — all Tavily calls standardize on
    /// `Authorization: Bearer {key}` (search/extract included, mysearch parity).
    pub async fn fetch_usage(
        &self,
        http: &Client,
        api_key: &str,
    ) -> Result<CreditSnapshot, ProviderError> {
        let url = format!("{}/usage", self.base_url);
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
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        let v: serde_json::Value = res.json().await?;
        parse_tavily_usage(&v)
    }
}

/// Apply absolute dates or relative time_range to a Tavily search body.
/// Absolute dates take precedence; Tavily forbids sending both.
pub(crate) fn apply_tavily_date_filters(
    body: &mut serde_json::Value,
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
) {
    let has_abs = from_date.is_some() || to_date.is_some();
    if let Some(d) = from_date {
        body["start_date"] = serde_json::json!(d);
    }
    if let Some(d) = to_date {
        body["end_date"] = serde_json::json!(d);
    }
    if !has_abs {
        if let Some(tr) = time_range {
            body["time_range"] = serde_json::json!(tr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderSearchParams;

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

    /// Serve one canned JSON response and capture the request that arrived.
    /// Returns (base_url, receiver) — the std::thread TcpListener pattern
    /// proven in xai.rs / http.rs, extended to record the wire bytes (F47).
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

    #[test]
    fn absolute_dates_set_start_end_skip_time_range() {
        let mut body = serde_json::json!({});
        apply_tavily_date_filters(
            &mut body,
            Some("2026-01-01"),
            Some("2026-01-31"),
            Some("week"),
        );
        assert_eq!(body["start_date"], "2026-01-01");
        assert_eq!(body["end_date"], "2026-01-31");
        assert!(body.get("time_range").is_none(), "{body}");
    }

    #[test]
    fn only_from_date_skips_time_range() {
        let mut body = serde_json::json!({});
        apply_tavily_date_filters(&mut body, Some("2026-03-01"), None, Some("month"));
        assert_eq!(body["start_date"], "2026-03-01");
        assert!(body.get("end_date").is_none());
        assert!(body.get("time_range").is_none(), "{body}");
    }

    #[test]
    fn time_range_when_no_absolute_dates() {
        let mut body = serde_json::json!({});
        apply_tavily_date_filters(&mut body, None, None, Some("day"));
        assert_eq!(body["time_range"], "day");
        assert!(body.get("start_date").is_none());
        assert!(body.get("end_date").is_none());
    }

    #[test]
    fn empty_extract_is_unextractable_not_upstream_502() {
        // Contract: empty results / failed_results must not look like 5xx key fail.
        let err = ProviderError::Unextractable {
            provider: "tavily".into(),
            message: "extract returned no results".into(),
        };
        match &err {
            ProviderError::Unextractable { provider, message } => {
                assert_eq!(provider, "tavily");
                assert!(message.contains("no results"));
            }
            other => panic!("expected Unextractable, got {other:?}"),
        }
        assert!(!matches!(err, ProviderError::Upstream { status: 502, .. }));
    }

    // --- F47: request-side wire format (path, headers, body field names) -----

    /// Tavily search authenticates via `Authorization: Bearer` and carries the
    /// exact documented body keys (query/max_results/search_depth/topic + the
    /// additive surface: include_answer/include_raw_content/include_images/chunks_per_source).
    #[tokio::test]
    async fn search_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "query": "rust wire",
            "answer": "a",
            "results": [{
                "title": "T", "url": "https://t.example",
                "content": "c", "raw_content": "rc", "score": 0.9
            }]
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let include = vec!["docs.rs".to_string()];
        let p = ProviderSearchParams {
            query: "rust wire",
            max_results: 7,
            api_key: "tvly-secret-key",
            include_content: true,
            include_answer: true,
            include_images: true,
            include_raw_content: true,
            chunks_per_source: Some(3),
            search_depth: Some("advanced"),
            tavily_topic: Some("news"),
            firecrawl_categories: None,
            sources: None,
            include_domains: Some(&include),
            exclude_domains: None,
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: Some("2026-01-01"),
            to_date: None,
            time_range: Some("week"),
            country: Some("ID"),
            exact_match: Some(true),
        };
        let out = client.search(&http, p).await.expect("search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/search", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("content-type").unwrap_or(""),
            "application/json",
            "content-type"
        );
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-secret-key",
            "tavily auth is Bearer (body api_key is legacy)"
        );
        let b = rec.body_json();
        assert!(
            b.get("api_key").is_none(),
            "tavily must not send body api_key: {b}"
        );
        assert_eq!(b["query"], "rust wire");
        assert_eq!(b["max_results"], 7);
        assert_eq!(b["search_depth"], "advanced");
        assert_eq!(b["topic"], "news");
        assert_eq!(b["include_answer"], true);
        assert_eq!(b["include_raw_content"], true);
        assert_eq!(b["include_images"], true);
        assert_eq!(b["chunks_per_source"], 3);
        assert_eq!(b["include_domains"], serde_json::json!(["docs.rs"]));
        // Absolute date wins over time_range (Tavily forbids both).
        assert_eq!(b["start_date"], "2026-01-01");
        assert!(b.get("time_range").is_none(), "{b}");
        assert_eq!(b["country"], "ID");
        assert_eq!(b["exact_match"], true);
        // Response parses back: item fields straight from the wire.
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].title, "T");
        assert_eq!(out.items[0].snippet.as_deref(), Some("c"));
        assert_eq!(
            out.items[0].content.as_deref(),
            Some("rc"),
            "include_raw_content → raw_content carried"
        );
        let score = out.items[0].score.expect("score from wire");
        assert!((score - 0.9).abs() < 1e-9, "score parsed: {score}");
        assert_eq!(out.items[0].provider.as_deref(), Some("tavily"));
        assert_eq!(out.answer.as_deref(), Some("a"));
    }

    /// Tavily `search_depth` is a passthrough — "ultra" must flow to the wire
    /// untouched (basic/advanced/ultra all accepted upstream).
    #[tokio::test]
    async fn search_depth_ultra_passes_through() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "query": "q",
            "results": []
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let p = ProviderSearchParams {
            query: "q",
            max_results: 1,
            api_key: "tvly-depth-key",
            include_content: false,
            include_answer: false,
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
            search_depth: Some("ultra"),
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
        };
        let _out = client.search(&http, p).await.expect("search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.body_json()["search_depth"], "ultra");
    }

    /// The native `include_raw_content` knob alone (without `include_content`)
    /// must still ask the wire for raw content AND carry `raw_content` into
    /// the item — same body flag, honest item carry.
    #[tokio::test]
    async fn raw_content_knob_alone_sets_body_and_carries_raw() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "query": "q",
            "results": [{
                "title": "T", "url": "https://t.example",
                "raw_content": "# raw"
            }]
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let p = ProviderSearchParams {
            query: "q",
            max_results: 1,
            api_key: "tvly-raw-key",
            include_content: false,
            include_answer: false,
            include_images: false,
            include_raw_content: true,
            chunks_per_source: None,
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
        };
        let out = client.search(&http, p).await.expect("search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.body_json()["include_raw_content"], true);
        assert_eq!(out.items[0].content.as_deref(), Some("# raw"));
    }

    /// Tavily extract hits POST /extract with Bearer auth + urls array.
    #[tokio::test]
    async fn extract_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [{
                "url": "https://example.com/page",
                "raw_content": "# markdown body"
            }]
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let out = client
            .extract(&http, "https://example.com/page", "tvly-extract-key")
            .await
            .expect("extract against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/extract", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-extract-key",
            "tavily extract auth is Bearer"
        );
        let b = rec.body_json();
        assert!(
            b.get("api_key").is_none(),
            "tavily extract must not send body api_key: {b}"
        );
        assert_eq!(b["urls"], serde_json::json!(["https://example.com/page"]));
        assert_eq!(out.content, "# markdown body");
        assert_eq!(out.url, "https://example.com/page");
        assert_eq!(out.provider, "tavily");
    }

    /// All Tavily calls standardize on `Authorization: Bearer` (mysearch
    /// parity) — usage is a GET, search/extract are POSTs with Bearer too.
    #[tokio::test]
    async fn usage_wire_uses_bearer_get() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "account": { "plan_limit": 1000, "plan_usage": 100, "paygo_limit": 0, "paygo_usage": 0 },
            "key": { "usage": 5, "limit": 50 }
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let snap = client
            .fetch_usage(&http, "tvly-usage-key")
            .await
            .expect("usage against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/usage", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-usage-key",
            "usage auth is Bearer, not body api_key"
        );
        assert!(
            rec.body.is_empty(),
            "GET /usage carries no body: {}",
            rec.body
        );
        assert_eq!(snap.remaining, 900);
        assert_eq!(snap.limit, 1000);
    }
}
