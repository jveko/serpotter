use crate::{ProviderError, ProviderResult, ProviderSearchParams};
use reqwest::Client;

use serde::Deserialize;
use serpotter_core::SearchItem;

/// Thin Exa adapter — HTTP client is supplied per call (registry cache).
#[derive(Clone)]
pub struct ExaClient {
    base_url: String,
}

impl ExaClient {
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
        let url = format!("{}/search", self.base_url);
        let mut contents = serde_json::json!({ "highlights": true });
        if p.include_content {
            contents["text"] = serde_json::json!(true);
        }
        let mut body = serde_json::json!({
            "query": p.query,
            "numResults": p.max_results,
            "contents": contents,
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
        apply_exa_date_filters(&mut body, p.from_date, p.to_date, p.time_range);

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
                provider: "exa".into(),
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
            title: Option<String>,
            url: Option<String>,
            text: Option<String>,
            summary: Option<String>,
            highlights: Option<Vec<String>>,
            score: Option<f64>,
            #[serde(rename = "publishedDate")]
            published_date: Option<String>,
        }

        let up: Up = res.json().await?;
        let items = up
            .results
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                let snippet = r
                    .highlights
                    .map(|h| h.join(" ... "))
                    .filter(|s| !s.is_empty())
                    .or(r.summary)
                    .or(r.text.clone());
                SearchItem {
                    title: r.title.unwrap_or_default(),
                    url: r.url.unwrap_or_default(),
                    snippet,
                    content: if p.include_content { r.text } else { None },
                    score: r.score,
                    published: r.published_date,
                    author: None,
                    provider: Some("exa".into()),
                    source: Some("web".into()),
                }
            })
            .collect();

        Ok(ProviderResult {
            provider: "exa".into(),
            query: p.query.to_string(),
            items,
            answer: None,
        })
    }
}

/// Set Exa startPublishedDate / endPublishedDate.
/// Absolute dates win; when only `time_range` is set, map day/week/month/year to a
/// relative start date (UTC YYYY-MM-DD) so Exa does not silently drop the filter.
pub(crate) fn apply_exa_date_filters(
    body: &mut serde_json::Value,
    from_date: Option<&str>,
    to_date: Option<&str>,
    time_range: Option<&str>,
) {
    let has_abs = from_date.is_some() || to_date.is_some();
    if let Some(d) = from_date {
        body["startPublishedDate"] = serde_json::json!(d);
    }
    if let Some(d) = to_date {
        body["endPublishedDate"] = serde_json::json!(d);
    }
    if !has_abs {
        if let Some(start) = exa_start_from_time_range(time_range) {
            body["startPublishedDate"] = serde_json::json!(start);
        }
    }
}

/// Map relative time_range → start ISO date (UTC), approx month=30d year=365d.
fn exa_start_from_time_range(time_range: Option<&str>) -> Option<String> {
    let days = match time_range.map(str::trim)? {
        "day" => 1u64,
        "week" => 7,
        "month" => 30,
        "year" => 365,
        _ => return None,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let secs = now.as_secs().saturating_sub(days.saturating_mul(86_400));
    Some(unix_secs_to_ymd(secs))
}

/// Civil YYYY-MM-DD from Unix seconds (UTC). Howard Hinnant civil_from_days.
fn unix_secs_to_ymd(secs: u64) -> String {
    let z = (secs / 86_400) as i64;
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{:02}-{:02}", m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_dates_set_when_present() {
        let mut body = serde_json::json!({});
        apply_exa_date_filters(
            &mut body,
            Some("2026-02-01"),
            Some("2026-02-28"),
            Some("week"),
        );
        assert_eq!(body["startPublishedDate"], "2026-02-01");
        assert_eq!(body["endPublishedDate"], "2026-02-28");
    }

    #[test]
    fn published_dates_none_leaves_body() {
        let mut body = serde_json::json!({ "query": "q" });
        apply_exa_date_filters(&mut body, None, None, None);
        assert!(body.get("startPublishedDate").is_none());
        assert!(body.get("endPublishedDate").is_none());
        assert_eq!(body["query"], "q");
    }

    #[test]
    fn time_range_week_sets_start_when_no_abs() {
        let mut body = serde_json::json!({});
        apply_exa_date_filters(&mut body, None, None, Some("week"));
        let start = body["startPublishedDate"].as_str().expect("start");
        assert_eq!(start.len(), 10, "{start}");
        assert!(body.get("endPublishedDate").is_none());
    }

    #[test]
    fn abs_dates_skip_time_range() {
        let mut body = serde_json::json!({});
        apply_exa_date_filters(&mut body, Some("2026-01-01"), None, Some("year"));
        assert_eq!(body["startPublishedDate"], "2026-01-01");
        assert!(body.get("endPublishedDate").is_none());
    }

    #[test]
    fn unix_epoch_ymd() {
        assert_eq!(unix_secs_to_ymd(0), "1970-01-01");
        // 2026-07-26 00:00:00 UTC
        assert_eq!(unix_secs_to_ymd(1_785_052_800), "2026-07-26");
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

    /// Exa search authenticates via Bearer header and uses the camelCase body
    /// keys (numResults/contents/includeDomains/startPublishedDate).
    #[tokio::test]
    async fn search_wire_format_matches_current_contract() {
        let (base, rx) = spawn_recording_server(serde_json::json!({
            "results": [{
                "title": "T", "url": "https://t.example", "text": "body",
                "highlights": ["h1", "h2"], "score": 0.8, "publishedDate": "2026-01-01"
            }]
        }));
        let client = ExaClient::new(base);
        let http = crate::http::build_direct();
        let exclude = vec!["spam.example".to_string()];
        let p = ProviderSearchParams {
            query: "rust wire",
            max_results: 4,
            api_key: "exa-secret-key",
            include_content: true,
            include_answer: false,
            search_depth: None,
            tavily_topic: None,
            firecrawl_categories: None,
            sources: None,
            include_domains: None,
            exclude_domains: Some(&exclude),
            allowed_x_handles: None,
            excluded_x_handles: None,
            from_date: Some("2026-03-01"),
            to_date: Some("2026-03-31"),
            time_range: Some("month"),
            country: None,
            exact_match: None,
        };
        let out = client.search(&http, p).await.expect("search against mock");
        let rec = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("request recorded");
        assert_eq!(rec.path(), "/search", "path: {}", rec.request_line);
        assert_eq!(
            rec.header("authorization").unwrap_or(""),
            "Bearer exa-secret-key",
            "exa auth is Bearer"
        );
        let b = rec.body_json();
        assert_eq!(b["query"], "rust wire");
        assert_eq!(b["numResults"], 4);
        assert_eq!(b["contents"]["highlights"], true);
        assert_eq!(
            b["contents"]["text"], true,
            "include_content → contents.text"
        );
        assert_eq!(b["excludeDomains"], serde_json::json!(["spam.example"]));
        assert_eq!(b["startPublishedDate"], "2026-03-01");
        assert_eq!(b["endPublishedDate"], "2026-03-31");
        // Response parses back: highlights joined as the snippet, text carried.
        assert_eq!(out.items.len(), 1);
        let item = &out.items[0];
        assert_eq!(item.title, "T");
        assert_eq!(item.snippet.as_deref(), Some("h1 ... h2"));
        assert_eq!(item.content.as_deref(), Some("body"));
        assert_eq!(item.published.as_deref(), Some("2026-01-01"));
        let score = item.score.expect("score from wire");
        assert!((score - 0.8).abs() < 1e-9, "score parsed: {score}");
        assert_eq!(item.provider.as_deref(), Some("exa"));
    }
}
