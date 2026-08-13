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

/// Firecrawl v2 scrape response-cache age (repeat scrapes within the window
/// are served from cache — cheap for research/extract re-reads).
const SCRAPE_MAX_AGE: &str = "2d";

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
            // ESTIMATE: Firecrawl v2 search is 1 credit (no per-call usage in
            // the response); tokens are not exposed.
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost: Some(1.0),
        })
    }

    /// Scrape page content via Firecrawl `/v2/scrape` (markdown + html).
    ///
    /// v2 wraps the URL in an array, keeps formats minimal (markdown + html)
    /// and reuses the response cache up to [`SCRAPE_MAX_AGE`] — the legacy
    /// `/v1/scrape` path is deprecated upstream. The parsed page shape is
    /// unchanged, so product/extract callers are not affected.
    pub async fn extract(
        &self,
        http: &Client,
        url: &str,
        api_key: &str,
    ) -> Result<ExtractResult, ProviderError> {
        let endpoint = format!("{}/v2/scrape", self.base_url);
        let body = serde_json::json!({
            "urls": [url],
            "formats": ["markdown", "html"],
            "maxAge": SCRAPE_MAX_AGE,
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
            #[allow(dead_code)]
            success: Option<bool>,
            data: Option<Data>,
            error: Option<String>,
        }
        #[derive(Deserialize)]
        struct Data {
            markdown: Option<String>,
            html: Option<String>,
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
        let Some(data) = up.data else {
            let msg = up.error.unwrap_or_else(|| "scrape returned no data".into());
            return Err(ProviderError::Unextractable {
                provider: "firecrawl".into(),
                message: msg,
            });
        };
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
            content: data.markdown.or(data.html).unwrap_or_default(),
            provider: "firecrawl".into(),
            // ESTIMATE: /v2/scrape is 1 credit (no per-call usage in the response).
            cost: Some(1.0),
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

    /// Start a structured (schema/prompt-driven) extraction job via
    /// `/v2/extract`. Async by design: the vendor returns a job `id` and the
    /// caller polls [`Self::structured_status`] until terminal. `prompt` and
    /// `schema` are optional (upstream expects at least one); absent fields
    /// are omitted from the body; `enableWebSearch` is pinned false so the
    /// extraction stays on the provided URLs.
    pub async fn extract_structured(
        &self,
        http: &Client,
        urls: &[String],
        prompt: Option<&str>,
        schema: Option<&serde_json::Value>,
        api_key: &str,
    ) -> Result<StructuredJob, ProviderError> {
        let endpoint = format!("{}/v2/extract", self.base_url);
        let mut body = serde_json::json!({
            "urls": urls,
            "enableWebSearch": false,
        });
        if let Some(p) = prompt {
            body["prompt"] = serde_json::json!(p);
        }
        if let Some(s) = schema {
            body["schema"] = s.clone();
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
                provider: "firecrawl".into(),
                status: status.as_u16(),
                body: text,
            });
        }
        #[derive(Deserialize)]
        struct Start {
            #[allow(dead_code)]
            success: Option<bool>,
            id: Option<String>,
            error: Option<serde_json::Value>,
        }
        let start: Start = res.json().await?;
        match start.id {
            Some(id) => Ok(StructuredJob { id }),
            None => {
                let msg = start
                    .error
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "no job id in /v2/extract response".into());
                Err(ProviderError::Upstream {
                    provider: "firecrawl".into(),
                    status: status.as_u16(),
                    body: msg,
                })
            }
        }
    }

    /// Poll a structured-extraction job. Maps vendor statuses honestly:
    /// `completed` and `failed` (+ `cancelled`, + explicit `success:false`)
    /// are terminal; anything else (e.g. `processing`) is neither. `data` is
    /// present only after completion; `error` carries the vendor message.
    pub async fn structured_status(
        &self,
        http: &Client,
        id: &str,
        api_key: &str,
    ) -> Result<StructuredStatus, ProviderError> {
        let endpoint = format!("{}/v2/extract/{id}", self.base_url);
        let res = http
            .get(&endpoint)
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
        #[derive(Deserialize)]
        struct Status {
            success: Option<bool>,
            status: Option<String>,
            data: Option<serde_json::Value>,
            error: Option<serde_json::Value>,
        }
        let st: Status = res.json().await?;
        let raw = st.status.as_deref().unwrap_or("");
        Ok(StructuredStatus {
            completed: raw.eq_ignore_ascii_case("completed"),
            failed: raw.eq_ignore_ascii_case("failed")
                || raw.eq_ignore_ascii_case("cancelled")
                || st.success == Some(false),
            data: st.data,
            error: st.error.map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            }),
        })
    }

    /// Extract the answer to one question from a single URL via Firecrawl
    /// `POST /v2/extract` (B27) — reuses the B18 job machinery.
    ///
    /// Firecrawl `/v2/extract` is ASYNC: start the job (single URL + the
    /// question as the extraction `prompt`, `enableWebSearch` pinned false so
    /// the extraction stays on the provided URL), then poll
    /// [`Self::structured_status`] every 1.5s up to 60s. A completed job
    /// returns its structured `data`; a failed/cancelled job or a poll
    /// timeout is URL-class [`ProviderError::Unextractable`] — never a fake
    /// HTTP health signal — so product can continue the extract chain.
    pub async fn extract_question(
        &self,
        http: &Client,
        api_key: &str,
        url: &str,
        question: &str,
    ) -> Result<serde_json::Value, ProviderError> {
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1500);
        const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

        let urls = [url.to_string()];
        let job = self
            .extract_structured(http, &urls, Some(question), None, api_key)
            .await?;
        let deadline = std::time::Instant::now() + POLL_DEADLINE;
        loop {
            let st = self.structured_status(http, &job.id, api_key).await?;
            if st.completed {
                return st.data.ok_or_else(|| ProviderError::Unextractable {
                    provider: "firecrawl".into(),
                    message: "question extraction completed but carried no data".into(),
                });
            }
            if st.failed {
                return Err(ProviderError::Unextractable {
                    provider: "firecrawl".into(),
                    message: st
                        .error
                        .unwrap_or_else(|| "question extraction job failed".into()),
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(ProviderError::Unextractable {
                    provider: "firecrawl".into(),
                    message: "question extraction did not complete within 60s".into(),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// Handle returned by `POST /v2/extract` — poll with
/// [`FirecrawlClient::structured_status`].
#[derive(Debug, Clone)]
pub struct StructuredJob {
    pub id: String,
}

/// Poll result for a Firecrawl structured-extraction job.
#[derive(Debug, Clone)]
pub struct StructuredStatus {
    pub completed: bool,
    pub failed: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
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
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
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
            include_images: false,
            include_raw_content: false,
            chunks_per_source: None,
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
        // v2 search → 1-credit ESTIMATE
        let cost = out.cost.expect("firecrawl cost estimate");
        assert!((cost - 1.0).abs() < 1e-9, "search = 1 credit: {cost}");
        assert!(out.input_tokens.is_none() && out.output_tokens.is_none());
    }

    /// Firecrawl extract (B21) is POST /v2/scrape with Bearer + the v2
    /// urls/formats/maxAge body, and parses the v2 data shape.
    #[tokio::test]
    async fn extract_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "success": true,
            "data": {
                "markdown": "# md",
                "html": "<h1>md</h1>",
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
        assert_eq!(rec.path(), "/v2/scrape", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer fc-extract-key",
            "firecrawl extract auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["urls"], serde_json::json!(["https://example.com/page"]));
        assert_eq!(b["formats"], serde_json::json!(["markdown", "html"]));
        assert_eq!(b["maxAge"], "2d");
        assert!(b.get("url").is_none(), "v2 wraps the url in urls[]: {b}");
        assert_eq!(out.content, "# md");
        assert_eq!(out.title.as_deref(), Some("Page"));
        assert_eq!(
            out.url, "https://example.com/page",
            "sourceURL wins over input"
        );
        assert_eq!(out.provider, "firecrawl");
        // /v2/scrape → 1-credit ESTIMATE
        let cost = out.cost.expect("firecrawl extract cost estimate");
        assert!((cost - 1.0).abs() < 1e-9, "scrape = 1 credit: {cost}");
    }

    /// A v2 scrape that returns an error body (200, no data) is URL-class
    /// Unextractable — never an HTTP health signal.
    #[tokio::test]
    async fn extract_v2_error_body_is_unextractable() {
        let (base, _rx) = spawn_recording_server(serde_json::json!({
            "success": false,
            "error": "blocked by robots"
        }));
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let err = client
            .extract(&http, "https://example.com/page", "fc-key")
            .await
            .expect_err("vendor error body");
        match err {
            ProviderError::Unextractable { provider, message } => {
                assert_eq!(provider, "firecrawl");
                assert!(message.contains("blocked"), "{message}");
            }
            other => panic!("expected Unextractable, got {other:?}"),
        }
    }

    /// Firecrawl /v2/extract start (B18): POST path, Bearer, urls + prompt +
    /// schema + enableWebSearch=false, and the job id parses back.
    #[tokio::test]
    async fn extract_structured_start_wire_format() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "success": true,
            "id": "job-123"
        }));
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let urls = vec!["https://example.com".to_string()];
        let schema =
            serde_json::json!({ "type": "object", "properties": { "name": { "type": "string" } } });
        let job = client
            .extract_structured(
                &http,
                &urls,
                Some("extract the company name"),
                Some(&schema),
                "fc-struct-key",
            )
            .await
            .expect("start against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/v2/extract", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer fc-struct-key",
            "firecrawl structured auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["urls"], serde_json::json!(["https://example.com"]));
        assert_eq!(b["prompt"], "extract the company name");
        assert_eq!(b["schema"]["properties"]["name"]["type"], "string");
        assert_eq!(b["enableWebSearch"], false);
        assert_eq!(job.id, "job-123");
    }

    /// Optional fields are omitted from the start body when absent; a missing
    /// job id is a vendor-side error, not a client success.
    #[tokio::test]
    async fn extract_structured_start_omits_absent_fields() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "success": true,
            "id": "job-456"
        }));
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let urls = vec!["https://example.com".to_string()];
        let job = client
            .extract_structured(&http, &urls, None, None, "fc-struct-key")
            .await
            .expect("start against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        let b = rec.body_json();
        assert!(b.get("prompt").is_none(), "{b}");
        assert!(b.get("schema").is_none(), "{b}");
        assert_eq!(b["enableWebSearch"], false);
        assert_eq!(job.id, "job-456");
    }

    /// /v2/extract/{id} polling maps vendor statuses honestly: completed
    /// carries data; processing is neither terminal; failed carries error.
    #[tokio::test]
    async fn structured_status_maps_completed_processing_failed() {
        let http = crate::http::build_direct();

        // completed → data
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "success": true,
            "status": "completed",
            "data": { "company": "Acme" }
        }));
        let client = FirecrawlClient::new(base);
        let st = client
            .structured_status(&http, "job-1", "fc-struct-key")
            .await
            .expect("status against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(
            rec.path(),
            "/v2/extract/job-1",
            "path: {}",
            rec.request_line
        );
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer fc-struct-key",
            "structured status auth is Bearer"
        );
        assert!(st.completed, "{st:?}");
        assert!(!st.failed, "{st:?}");
        assert_eq!(
            st.data.as_ref().and_then(|d| d.get("company")),
            Some(&serde_json::json!("Acme"))
        );
        assert!(st.error.is_none(), "{st:?}");

        // processing → neither terminal
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "success": true,
            "status": "processing"
        }));
        let client = FirecrawlClient::new(base);
        let st = client
            .structured_status(&http, "job-1", "fc-struct-key")
            .await
            .expect("status against mock");
        let _rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert!(!st.completed, "{st:?}");
        assert!(!st.failed, "{st:?}");
        assert!(st.data.is_none(), "{st:?}");

        // failed (success:false + error) → failed with the vendor message
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "success": false,
            "status": "failed",
            "error": "credits exhausted"
        }));
        let client = FirecrawlClient::new(base);
        let st = client
            .structured_status(&http, "job-1", "fc-struct-key")
            .await
            .expect("status against mock");
        let _rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert!(!st.completed, "{st:?}");
        assert!(st.failed, "{st:?}");
        assert!(st.data.is_none(), "{st:?}");
        assert_eq!(st.error.as_deref(), Some("credits exhausted"));
    }
    // ---- B27: /v2/extract question ----

    /// Two-shot loopback server: serves `start` on the first connection and
    /// `status` on the second (the extract_question start→poll flow).
    fn spawn_two_shot_server(
        start: serde_json::Value,
        status: serde_json::Value,
    ) -> (String, std::sync::mpsc::Receiver<RecordedRequest>) {
        use std::io::Write;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = std::sync::mpsc::channel();

        fn drain_request(stream: &mut std::net::TcpStream) -> RecordedRequest {
            use std::io::Read;
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
            RecordedRequest {
                request_line,
                headers,
                body: body_part.to_string(),
            }
        }

        std::thread::spawn(move || {
            // Connection 1: start request → start response.
            if let Ok((mut s1, _)) = listener.accept() {
                let rec = drain_request(&mut s1);
                let _ = tx.send(rec);
                let body = serde_json::to_string(&start).expect("serialize start");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s1.write_all(resp.as_bytes());
            }
            // Connection 2: status request → status response.
            if let Ok((mut s2, _)) = listener.accept() {
                let rec = drain_request(&mut s2);
                let _ = tx.send(rec);
                let body = serde_json::to_string(&status).expect("serialize status");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = s2.write_all(resp.as_bytes());
            }
        });
        (format!("http://{addr}"), rx)
    }

    /// extract_question starts a single-URL /v2/extract job with the question
    /// as the prompt (enableWebSearch pinned false), polls to completion and
    /// returns the structured data.
    #[tokio::test]
    async fn extract_question_start_and_poll_returns_data() {
        let (base, rx) = spawn_two_shot_server(
            serde_json::json!({ "success": true, "id": "job-q1" }),
            serde_json::json!({
                "success": true,
                "status": "completed",
                "data": { "answer": "42" }
            }),
        );
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let data = client
            .extract_question(
                &http,
                "fc-q-key",
                "https://example.com/page",
                "What is the answer?",
            )
            .await
            .expect("question extraction against mock");
        // Start request wire: /v2/extract, urls + prompt + enableWebSearch=false.
        let start_rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("start request recorded");
        assert_eq!(
            start_rec.path(),
            "/v2/extract",
            "path: {}",
            start_rec.request_line
        );
        assert_eq!(
            start_rec.header("authorization").unwrap_or(""),
            "Bearer fc-q-key",
            "extract_question auth is Bearer"
        );
        let b = start_rec.body_json();
        assert_eq!(b["urls"], serde_json::json!(["https://example.com/page"]));
        assert_eq!(b["prompt"], "What is the answer?");
        assert_eq!(b["enableWebSearch"], false);
        // Status request wire: GET /v2/extract/job-q1.
        let status_rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("status request recorded");
        assert_eq!(
            status_rec.path(),
            "/v2/extract/job-q1",
            "path: {}",
            status_rec.request_line
        );
        assert_eq!(data["answer"], "42");
    }

    /// A failed job surfaces as URL-class Unextractable with the vendor
    /// message — never a fake HTTP health signal.
    #[tokio::test]
    async fn extract_question_failed_job_is_unextractable() {
        let (base, _rx) = spawn_two_shot_server(
            serde_json::json!({ "success": true, "id": "job-q2" }),
            serde_json::json!({
                "success": false,
                "status": "failed",
                "error": "page blocked the extractor"
            }),
        );
        let client = FirecrawlClient::new(base);
        let http = crate::http::build_direct();
        let err = client
            .extract_question(&http, "fc-q-key", "https://example.com/page", "Q?")
            .await
            .expect_err("failed job");
        match err {
            ProviderError::Unextractable { provider, message } => {
                assert_eq!(provider, "firecrawl");
                assert!(message.contains("blocked"), "{message}");
            }
            other => panic!("expected Unextractable, got {other:?}"),
        }
    }
}
