use crate::{
    parse_tavily_usage, CreditSnapshot, ExtractResult, ProviderError, ProviderResult,
    ProviderSearchParams,
};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

const DEFAULT: &str = "https://api.tavily.com";

/// Tavily `/extract` documents a 20-URL cap per call (docs + SDK). Exceeding
/// it is refused locally via [`ProviderError::Unsupported`] — the crate's
/// convention for upstream parameter caps — instead of a vendor 400.
const TAVILY_EXTRACT_MAX_URLS: usize = 20;

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
        // B2/B22: Tavily search exposes no per-call usage, so cost is an
        // ESTIMATE in credits by search_depth (basic/advanced/ultra = 1/2/3).
        let credits = match p.search_depth.unwrap_or("basic") {
            "ultra" => 3u64,
            "advanced" => 2,
            _ => 1,
        };
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
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost: Some(credits as f64),
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
                // ESTIMATE: Tavily /extract has no per-call usage surface; 1 credit.
                cost: Some(1.0),
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

    /// Start an asynchronous Tavily research task (B17) via `POST /research`.
    ///
    /// Wire (verified against docs.tavily.com/api-reference/endpoint/research
    /// and the official tavily-python SDK): the query rides in `input` (NOT
    /// `query`), `citation_format` is one of numbered|mla|apa|chicago, `model`
    /// is the research agent (auto|mini|pro), and the response carries the job
    /// id in `request_id` (NOT `id`) plus a `status`. `max_depth` has no named
    /// SDK parameter, but the official SDK forwards unknown kwargs straight
    /// into the body (the docs example passes include_domains/output_length
    /// the same way) — so when `Some` it is forwarded as `max_depth`.
    ///
    /// Poll completion via [`Self::research_status`] (J3 polls every 2s up to
    /// min(request_timeout, 90s) per the Wave 3B design).
    pub async fn research(
        &self,
        http: &Client,
        api_key: &str,
        query: &str,
        max_depth: Option<u32>,
        citation_format: Option<&str>,
        model: Option<&str>,
    ) -> Result<TavilyResearchJob, ProviderError> {
        let url = format!("{}/research", self.base_url);
        let mut body = serde_json::json!({
            "input": query,
            "stream": false,
        });
        if let Some(d) = max_depth {
            body["max_depth"] = serde_json::json!(d);
        }
        if let Some(c) = citation_format {
            body["citation_format"] = serde_json::json!(c);
        }
        if let Some(m) = model {
            body["model"] = serde_json::json!(m);
        }
        let res = http
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
                provider: "tavily".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Start {
            request_id: Option<String>,
            status: Option<String>,
        }
        let start: Start = res.json().await?;
        match start.request_id {
            // `status: Some("pending"|"queued")` is the healthy start signal;
            // absence of a job id is a vendor-side failure, not a client win.
            Some(id) if !id.is_empty() => Ok(TavilyResearchJob { id }),
            _ => {
                let raw = serde_json::json!({
                    "status": start.status,
                    "detail": "no request_id in /research response",
                });
                Err(ProviderError::Upstream {
                    provider: "tavily".into(),
                    status: status.as_u16(),
                    body: raw.to_string(),
                })
            }
        }
    }

    /// Poll one Tavily research task via `GET /research/{id}`.
    ///
    /// Single shot, designed for J3's bounded poll loop: `completed`/`failed`
    /// are terminal (status "completed"/"failed"); anything else (pending,
    /// running, queued, absent) maps to neither and the caller should re-poll.
    /// The report text rides in `content` and the source list in `sources`
    /// (per the official SDK's get_research shape). Both 200 and 202 are
    /// treated as "have a current state" (mirrors the SDK).
    pub async fn research_status(
        &self,
        http: &Client,
        api_key: &str,
        id: &str,
    ) -> Result<TavilyResearchStatus, ProviderError> {
        let url = format!("{}/research/{id}", self.base_url);
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
        #[derive(Deserialize)]
        struct Up {
            status: Option<String>,
            content: Option<String>,
            sources: Option<Vec<Source>>,
        }
        #[derive(Deserialize)]
        struct Source {
            title: Option<String>,
            url: Option<String>,
        }
        let up: Up = res.json().await?;
        let raw = up.status.as_deref().unwrap_or("");
        let completed = raw.eq_ignore_ascii_case("completed");
        let failed = raw.eq_ignore_ascii_case("failed");
        let answer = up.content.filter(|c| !c.trim().is_empty());
        let citations = up.sources.and_then(|sources| {
            let cites: Vec<TavilyCitation> = sources
                .into_iter()
                .filter_map(|s| {
                    let url = s.url?;
                    Some(TavilyCitation {
                        title: s.title.unwrap_or_default(),
                        url,
                    })
                })
                .collect();
            (!cites.is_empty()).then_some(cites)
        });
        Ok(TavilyResearchStatus {
            completed,
            failed,
            answer,
            citations,
        })
    }

    /// Extract multiple URLs in one call via Tavily `/extract` (B26).
    ///
    /// `format` is the CURRENT documented /extract enum — `markdown` or
    /// `text` — or `None` for the vendor default. The Wave 3B contract's
    /// `question`/`highlights` values are NOT expressible on Tavily's wire
    /// (verified current docs: format is only markdown|text; question-focused
    /// extraction is a separate `query` body field, and there is no
    /// highlights mode); asking for them returns [`ProviderError::Unsupported`]
    /// BEFORE any network call so product can route to the exa/firecrawl legs.
    /// Batch semantics: every result row with content becomes a
    /// [`TavilyExtractedPage`]; failed URLs (reported in `failed_results`)
    /// are simply absent from the returned list — one bad URL never fails the
    /// whole batch.
    pub async fn extract_batch(
        &self,
        http: &Client,
        api_key: &str,
        urls: &[String],
        format: Option<&str>,
    ) -> Result<Vec<TavilyExtractedPage>, ProviderError> {
        match format {
            None | Some("markdown") | Some("text") => {}
            Some(other) => {
                return Err(ProviderError::Unsupported {
                    provider: "tavily".into(),
                    action: "extract",
                    detail: format!(
                        "format={other} is not supported by Tavily /extract (documented values: markdown, text); question/highlights extraction are not expressible on Tavily — route to the exa/firecrawl legs"
                    ),
                });
            }
        }
        if urls.len() > TAVILY_EXTRACT_MAX_URLS {
            return Err(ProviderError::Unsupported {
                provider: "tavily".into(),
                action: "extract",
                detail: format!(
                    "Tavily /extract caps at {TAVILY_EXTRACT_MAX_URLS} URLs per call, got {}",
                    urls.len()
                ),
            });
        }
        let endpoint = format!("{}/extract", self.base_url);
        let mut body = serde_json::json!({ "urls": urls });
        if let Some(f) = format {
            body["format"] = serde_json::json!(f);
        }
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
        }
        #[derive(Deserialize)]
        struct Row {
            url: Option<String>,
            raw_content: Option<String>,
            content: Option<String>,
        }
        let up: Up = res.json().await?;
        Ok(up
            .results
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| {
                let content = r.raw_content.or(r.content)?;
                if content.trim().is_empty() {
                    return None;
                }
                Some(TavilyExtractedPage {
                    url: r.url.unwrap_or_default(),
                    content,
                })
            })
            .collect())
    }

    /// Discover the URL list of a site via Tavily `POST /map` (B25).
    ///
    /// Tavily DOES ship an official `/map` endpoint (docs.tavily.com/
    /// api-reference/endpoint/map — verified 2026-08); the Wire 3B design's
    /// skip-condition ("if the Tavily client has no map endpoint") does not
    /// hold, so both Tavily and Firecrawl map adapters are implemented. The
    /// response is `{ results: [url, ...] }` — plain URL strings, unlike
    /// Firecrawl's `links: [{url, ...}]` objects — so the extraction differs
    /// per provider even though both return `Vec<String>`.
    pub async fn map_site(
        &self,
        http: &Client,
        api_key: &str,
        url: &str,
        limit: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        let endpoint = format!("{}/map", self.base_url);
        let mut body = serde_json::json!({ "url": url });
        if let Some(l) = limit {
            body["limit"] = serde_json::json!(l);
        }
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
            results: Option<Vec<String>>,
        }
        let up: Up = res.json().await?;
        Ok(up.results.unwrap_or_default())
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

/// Handle returned by `POST /research` (B17) — poll with
/// [`TavilyClient::research_status`].
#[derive(Debug, Clone)]
pub struct TavilyResearchJob {
    pub id: String,
}

/// Poll result for a Tavily research task. `completed`/`failed` are terminal;
/// neither means "still running, re-poll". `answer` carries the report text
/// and `citations` the source list (both `None` while running).
#[derive(Debug, Clone, Default)]
pub struct TavilyResearchStatus {
    pub completed: bool,
    pub failed: bool,
    pub answer: Option<String>,
    pub citations: Option<Vec<TavilyCitation>>,
}

/// One cited source of a completed Tavily research report.
#[derive(Debug, Clone)]
pub struct TavilyCitation {
    pub title: String,
    pub url: String,
}

/// One extracted page of a Tavily `/extract` batch call (B26).
#[derive(Debug, Clone)]
pub struct TavilyExtractedPage {
    pub url: String,
    pub content: String,
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
        // search_depth=advanced → 2-credit ESTIMATE
        let cost = out.cost.expect("tavily cost estimate");
        assert!(
            (cost - 2.0).abs() < 1e-9,
            "advanced depth = 2 credits: {cost}"
        );
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
        // ultra depth → 3-credit ESTIMATE
        let cost = _out.cost.expect("tavily cost estimate");
        assert!((cost - 3.0).abs() < 1e-9, "ultra depth = 3 credits: {cost}");
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
        // default (basic) depth → 1-credit ESTIMATE
        let cost = out.cost.expect("tavily cost estimate");
        assert!((cost - 1.0).abs() < 1e-9, "basic depth = 1 credit: {cost}");
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
        // /extract → 1-credit ESTIMATE (no per-call usage surface)
        let cost = out.cost.expect("tavily extract cost estimate");
        assert!((cost - 1.0).abs() < 1e-9, "extract = 1 credit: {cost}");
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

    // ---- B17: /research start + status (canned wire) ----

    /// Tavily research starts via POST /research with Bearer auth, the query
    /// in `input` (not `query`), `stream: false`, and citation_format/model
    /// passthrough; the job id parses from `request_id` (not `id`).
    #[tokio::test]
    async fn research_start_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "request_id": "res-123",
            "status": "pending",
            "created_at": "2026-08-11T00:00:00Z",
            "input": "what is quantum computing?",
            "model": "pro"
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let job = client
            .research(
                &http,
                "tvly-research-key",
                "what is quantum computing?",
                Some(2),
                Some("numbered"),
                Some("pro"),
            )
            .await
            .expect("research start against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/research", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-research-key",
            "research auth is Bearer"
        );
        assert_eq!(rec.header("content-type").unwrap_or(""), "application/json");
        let b = rec.body_json();
        assert_eq!(b["input"], "what is quantum computing?");
        assert!(
            b.get("query").is_none(),
            "research query rides in input: {b}"
        );
        assert_eq!(b["stream"], false);
        assert_eq!(b["citation_format"], "numbered");
        assert_eq!(b["model"], "pro");
        assert_eq!(
            b["max_depth"], 2,
            "max_depth forwarded per SDK kwargs convention"
        );
        assert_eq!(job.id, "res-123");
    }

    /// Defaults: no citation_format/model/max_depth keys when all None.
    #[tokio::test]
    async fn research_start_omits_absent_fields() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "request_id": "res-456",
            "status": "pending"
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let job = client
            .research(&http, "tvly-research-key", "q", None, None, None)
            .await
            .expect("research start against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        let b = rec.body_json();
        assert!(b.get("citation_format").is_none(), "{b}");
        assert!(b.get("model").is_none(), "{b}");
        assert!(b.get("max_depth").is_none(), "{b}");
        assert_eq!(b["input"], "q");
        assert_eq!(job.id, "res-456");
    }

    /// A start response without request_id is a vendor failure, not a win.
    #[tokio::test]
    async fn research_start_without_id_errors() {
        let (base, _rx) = spawn_recording_server(serde_json::json!({
            "detail": { "error": "Error when executing research task" }
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let err = client
            .research(&http, "tvly-research-key", "q", None, None, None)
            .await
            .expect_err("missing request_id");
        match err {
            ProviderError::Upstream {
                provider, status, ..
            } => {
                assert_eq!(provider, "tavily");
                assert_eq!(status, 200, "vendor returned 200 without a job id");
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    /// GET /research/{id} status polling: completed → answer + citations;
    /// running → neither terminal; failed → failed with no answer.
    #[tokio::test]
    async fn research_status_maps_completed_running_failed() {
        let http = crate::http::build_direct();

        // completed carries content + sources
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "request_id": "res-123",
            "status": "completed",
            "content": "Quantum computing uses qubits.",
            "sources": [
                { "url": "https://a.example", "title": "Source A" },
                { "url": "https://b.example", "title": "Source B" }
            ]
        }));
        let client = TavilyClient::new(base);
        let st = client
            .research_status(&http, "tvly-research-key", "res-123")
            .await
            .expect("status against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(
            rec.path(),
            "/research/res-123",
            "path: {}",
            rec.request_line
        );
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-research-key",
            "status auth is Bearer"
        );
        assert!(rec.body.is_empty(), "GET status carries no body");
        assert!(st.completed, "{st:?}");
        assert!(!st.failed, "{st:?}");
        assert_eq!(st.answer.as_deref(), Some("Quantum computing uses qubits."));
        let cites = st.citations.expect("citations from sources");
        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0].title, "Source A");
        assert_eq!(cites[0].url, "https://a.example");

        // running → neither terminal, no answer
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "request_id": "res-123",
            "status": "running"
        }));
        let client = TavilyClient::new(base);
        let st = client
            .research_status(&http, "tvly-research-key", "res-123")
            .await
            .expect("status against mock");
        let _rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert!(!st.completed, "{st:?}");
        assert!(!st.failed, "{st:?}");
        assert!(st.answer.is_none(), "{st:?}");
        assert!(st.citations.is_none(), "{st:?}");

        // failed → terminal failure
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "request_id": "res-123",
            "status": "failed"
        }));
        let client = TavilyClient::new(base);
        let st = client
            .research_status(&http, "tvly-research-key", "res-123")
            .await
            .expect("status against mock");
        let _rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert!(!st.completed, "{st:?}");
        assert!(st.failed, "{st:?}");
        assert!(st.answer.is_none(), "{st:?}");
    }

    // ---- B26/B27: /extract batch ----

    /// extract_batch posts urls[] (no format key when None) and maps every
    /// result row to a page; per-row content is raw_content-first.
    #[tokio::test]
    async fn extract_batch_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [
                { "url": "https://a.example", "raw_content": "# page a" },
                { "url": "https://b.example", "raw_content": "" }
            ],
            "failed_results": []
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let urls = vec![
            "https://a.example".to_string(),
            "https://b.example".to_string(),
        ];
        let pages = client
            .extract_batch(&http, "tvly-batch-key", &urls, None)
            .await
            .expect("batch extract against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/extract", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-batch-key",
            "batch auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(
            b["urls"],
            serde_json::json!(["https://a.example", "https://b.example"])
        );
        assert!(b.get("format").is_none(), "no format key when None: {b}");
        assert_eq!(
            pages.len(),
            1,
            "empty-content row is dropped, batch survives"
        );
        assert_eq!(pages[0].url, "https://a.example");
        assert_eq!(pages[0].content, "# page a");
    }

    /// Documented format values (markdown|text) pass through to the wire.
    #[tokio::test]
    async fn extract_batch_format_markdown_passes_through() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [{ "url": "https://a.example", "raw_content": "text" }]
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let urls = vec!["https://a.example".to_string()];
        let _pages = client
            .extract_batch(&http, "tvly-batch-key", &urls, Some("markdown"))
            .await
            .expect("batch extract against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.body_json()["format"], "markdown");
    }

    /// The Wave 3B contract's format=question|highlights are NOT expressible
    /// on Tavily's current /extract wire (format is markdown|text only) — the
    /// client refuses locally instead of inventing an unsupported value.
    #[tokio::test]
    async fn extract_batch_question_highlights_refused_before_network() {
        let client = TavilyClient::new("http://127.0.0.1:9");
        let http = crate::http::build_direct();
        let urls = vec!["https://a.example".to_string()];
        let err = client
            .extract_batch(&http, "tvly-batch-key", &urls, Some("question"))
            .await
            .expect_err("question format must be refused");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, "tavily");
                assert_eq!(action, "extract");
                assert!(detail.contains("question"), "{detail}");
                assert!(detail.contains("exa/firecrawl"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }

        let err = client
            .extract_batch(&http, "tvly-batch-key", &urls, Some("highlights"))
            .await
            .expect_err("highlights format must be refused");
        assert!(matches!(err, ProviderError::Unsupported { .. }), "{err:?}");
    }

    /// The documented 20-URL per-call cap is enforced locally (crate
    /// convention: upstream parameter caps -> Unsupported, never a vendor 400).
    #[tokio::test]
    async fn extract_batch_over_cap_refused_before_network() {
        let client = TavilyClient::new("http://127.0.0.1:9");
        let http = crate::http::build_direct();
        let urls: Vec<String> = (1..=21).map(|i| format!("https://u{i}.example")).collect();
        let err = client
            .extract_batch(&http, "tvly-batch-key", &urls, None)
            .await
            .expect_err("21 urls must be refused locally");
        match err {
            ProviderError::Unsupported {
                provider,
                action,
                detail,
            } => {
                assert_eq!(provider, "tavily");
                assert_eq!(action, "extract");
                assert!(detail.contains("20"), "{detail}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    // ---- B25: /map site discovery ----

    /// Tavily /map exists officially (verified 2026-08): POST with url (+
    /// optional limit), response `results: [url, ...]` — plain strings.
    #[tokio::test]
    async fn map_site_wire_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [
                "https://docs.tavily.com/",
                "https://docs.tavily.com/changelog"
            ]
        }));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let urls = client
            .map_site(&http, "tvly-map-key", "https://docs.tavily.com", Some(50))
            .await
            .expect("map against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/map", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer tvly-map-key",
            "map auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["url"], "https://docs.tavily.com");
        assert_eq!(b["limit"], 50);
        assert!(b.get("results").is_none(), "results is response-side: {b}");
        assert_eq!(
            urls,
            vec![
                "https://docs.tavily.com/".to_string(),
                "https://docs.tavily.com/changelog".to_string()
            ]
        );
    }

    /// Limit omitted → no limit key; missing results → empty list.
    #[tokio::test]
    async fn map_site_omits_limit_and_defaults_empty() {
        let (base, rx) = spawn_recording_server(serde_json::json!({}));
        let client = TavilyClient::new(base);
        let http = crate::http::build_direct();
        let urls = client
            .map_site(&http, "tvly-map-key", "https://example.com", None)
            .await
            .expect("map against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert!(rec.body_json().get("limit").is_none());
        assert!(urls.is_empty(), "no results -> empty vec");
    }
}
