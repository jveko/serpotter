//! Request events: the single funnel for every product request (search /
//! extract / research / MCP tools / failed auth). One event emits:
//!   1. a structured tracing log line  → the durable audit (stdout JSON logs)
//!   2. a ring-buffer entry            → admin /api/request-logs browser
//!   3. an error-window update         → cron high-error-rate alert
//!   4. a metrics observation          → /metrics
//!   5. a usage delta                  → SQLite usage_daily (write-time rollup,
//!      single writer task, drained on shutdown)
//!
//! The request_log table is gone (Task 2 migration 0017); raw per-request
//! events live only in the log stream. Nothing here ever fails the request
//! path.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::http::{request::Parts, HeaderMap};
use serpotter_auth::extract_token;
use serpotter_db::{Db, TokenRow};
use serpotter_product::ExecMeta;
use tokio::sync::{mpsc, Notify};

use crate::AppState;

/// One request event (service = vendor family; provider_used = dial label).
#[derive(Clone, Debug)]
pub struct LogFields {
    pub path: &'static str,
    pub status: i64,
    /// Request duration, filled by `emit` (ring rows keep it; the log line
    /// and metrics histogram use the same value).
    pub duration_ms: Option<i64>,
    pub service: Option<String>,
    pub provider_used: Option<String>,
    pub error_kind: Option<&'static str>,
    pub query_preview: Option<String>,
    pub request_id: Option<String>,
    pub token_name: Option<String>,
    pub strategy: Option<String>,
    pub providers_consulted: Option<String>,
    pub attempt_count: Option<i64>,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
    /// B2: input tokens from the successful provider call (NULL when unknown).
    pub input_tokens: Option<i64>,
    /// B2: output tokens from the successful provider call (NULL when unknown).
    pub output_tokens: Option<i64>,
    /// B2: total tokens (reported, else input+output sum).
    pub total_tokens: Option<i64>,
    /// B2: cost estimate (exact for Exa `costDollars`, credit estimates for
    /// Tavily/Firecrawl; NULL when unknown).
    pub cost_est: Option<f64>,
    /// B5: true when the response was served from the exact-query TTL cache
    /// (zero provider calls) — feeds `serpotter_cache_requests_total{hit}`.
    pub cache_hit: bool,
}

/// Truncate query/url preview to 120 chars for the log line + ring.
pub fn query_preview(s: &str) -> String {
    let mut out: String = s.chars().take(120).collect();
    if s.chars().count() > 120 {
        out.push('…');
    }
    out
}

/// Read `x-request-id` (SetRequestId already set it on the request before handlers).
pub fn request_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Multi-leg dial labels — never stored in `service`.
fn is_dial_label(s: &str) -> bool {
    matches!(s, "hybrid" | "blend" | "blend-verify" | "verify")
}

/// Vendor family for `service`: never hybrid/blend; first consulted on dial labels.
/// On bare meta (errors): last attempted vendor when `attempt_count > 0`.
pub fn service_from_meta(provider_used: Option<&str>, meta: &ExecMeta) -> Option<String> {
    if let Some(pu) = provider_used {
        if is_dial_label(pu) {
            return meta.providers_consulted.first().cloned();
        }
        return Some(pu.to_string());
    }
    if meta.attempt_count > 0 {
        meta.providers_consulted
            .last()
            .cloned()
            .or_else(|| meta.providers_consulted.first().cloned())
    } else {
        None
    }
}

/// Dial / route label for research rows. With F16 the `strategy` column stores
/// the RAW routed strategy (fast/balanced/verify/deep), so the research dial
/// label must be derived from it, matching search `provider_used`:
/// `verify` → `blend-verify` (3-leg), `balanced` → `blend` (2-leg),
/// anything else (fast/deep — single chains) → first consulted vendor.
pub fn research_dial_label(meta: &ExecMeta) -> Option<String> {
    match meta.strategy.as_deref() {
        Some("verify") => Some("blend-verify".into()),
        Some("balanced") => Some("blend".into()),
        _ => meta.providers_consulted.first().cloned(),
    }
}

/// Build log fields from product ExecMeta + dial label + auth/correlation.
#[allow(clippy::too_many_arguments)]
pub fn fields_from_meta(
    path: &'static str,
    status: i64,
    error_kind: Option<&'static str>,
    query_preview: Option<String>,
    request_id: Option<String>,
    token_name: Option<String>,
    provider_used: Option<String>,
    meta: &ExecMeta,
) -> LogFields {
    let service = service_from_meta(provider_used.as_deref(), meta);
    LogFields {
        path,
        status,
        duration_ms: None,
        service,
        provider_used,
        error_kind,
        query_preview,
        request_id,
        token_name,
        strategy: meta.strategy.clone(),
        providers_consulted: meta.providers_csv(),
        attempt_count: Some(i64::from(meta.attempt_count)),
        key_id: meta.key_id,
        node_id: meta.node_id,
        input_tokens: meta.input_tokens.map(|v| v as i64),
        output_tokens: meta.output_tokens.map(|v| v as i64),
        total_tokens: meta.total_tokens.map(|v| v as i64),
        cost_est: meta.cost,
        cache_hit: meta.cache_hit,
    }
}

/// Resolve MCP token_name + request_id from HTTP Parts (extensions + headers).
///
/// Prefers `TokenRow` stashed by mcp_auth_middleware; falls back to
/// `get_token_by_value` so valid tok- never leaves token_name NULL.
pub async fn resolve_mcp_log_ctx(db: &Db, parts: &Parts) -> (Option<String>, Option<String>) {
    let request_id = request_id_from_headers(&parts.headers);
    if let Some(row) = parts.extensions.get::<TokenRow>() {
        return (Some(row.name.clone()), request_id);
    }
    if let Some(tok) = extract_token(&parts.headers) {
        if let Ok(Some(row)) = db.get_token_by_value(&tok).await {
            return (Some(row.name), request_id);
        }
    }
    (None, request_id)
}

// --- Request ring: the admin "what just happened" browser ------------------

/// Bounded in-memory window of recent request events. Oldest entries are
/// evicted past [`RING_CAP`]. Lost on restart — the JSON log stream is the
/// durable record.
pub const RING_CAP: usize = 2048;

struct RingEntry {
    seq: u64,
    created_at: String,
    fields: LogFields,
}

/// One ring row as served by the admin endpoint (newest-first ordering by seq).
#[derive(Clone, Debug)]
pub struct RingEntryView {
    pub id: i64,
    pub created_at: String,
    pub fields: LogFields,
}

/// Admin list filters for the ring (mirrors the old `RequestLogFilter`).
#[derive(Clone, Debug, Default)]
pub struct RingFilter {
    pub limit: usize,
    pub offset: usize,
    pub status: Option<i64>,
    pub path_prefix: Option<String>,
    pub service: Option<String>,
    pub request_id: Option<String>,
    pub token_name: Option<String>,
}

/// Bounded FIFO of recent events, newest last; `list` returns newest first.
pub struct RequestRing {
    inner: Mutex<RingInner>,
}

struct RingInner {
    entries: VecDeque<RingEntry>,
    next_seq: u64,
}

impl RequestRing {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(RingInner {
                entries: VecDeque::new(),
                next_seq: 1,
            }),
        }
    }

    pub fn push(&self, fields: LogFields) {
        let mut inner = self.inner.lock().expect("ring mutex poisoned");
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.entries.push_back(RingEntry {
            seq,
            created_at: utc_now_str(),
            fields,
        });
        if inner.entries.len() > RING_CAP {
            inner.entries.pop_front();
        }
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .expect("ring mutex poisoned")
            .entries
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Newest-first rows matching the filter, paged by offset/limit.
    pub fn list(&self, filter: &RingFilter) -> Vec<RingEntryView> {
        let inner = self.inner.lock().expect("ring mutex poisoned");
        inner
            .entries
            .iter()
            .rev()
            .filter(|e| {
                filter.status.is_none_or(|s| e.fields.status == s)
                    && filter
                        .path_prefix
                        .as_deref()
                        .is_none_or(|p| e.fields.path.starts_with(p))
                    && filter
                        .service
                        .as_deref()
                        .is_none_or(|s| e.fields.service.as_deref() == Some(s))
                    && filter
                        .request_id
                        .as_deref()
                        .is_none_or(|r| e.fields.request_id.as_deref() == Some(r))
                    && filter
                        .token_name
                        .as_deref()
                        .is_none_or(|t| e.fields.token_name.as_deref() == Some(t))
            })
            .skip(filter.offset)
            .take(filter.limit)
            .map(|e| RingEntryView {
                id: e.seq as i64,
                created_at: e.created_at.clone(),
                fields: e.fields.clone(),
            })
            .collect()
    }
}

impl Default for RequestRing {
    fn default() -> Self {
        Self::new()
    }
}

// --- Error window: the high-error-rate alert source ------------------------

/// Sliding per-minute error-rate window for the high-error-rate alert.
/// In-memory only: an empty window after restart never fires a false alert
/// (it needs ALERT_MIN_TOTAL requests to accumulate first).
pub(crate) struct ErrorWindow {
    /// (epoch_minute, total, errors) buckets, oldest first.
    inner: Mutex<VecDeque<(i64, i64, i64)>>,
}

impl ErrorWindow {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
        }
    }

    /// Bucket one finished request into the current minute.
    pub(crate) fn record(&self, status: i64) {
        self.record_at(status, now_minute());
    }

    /// Test-visible core: bucket by an explicit epoch minute.
    pub(crate) fn record_at(&self, status: i64, minute: i64) {
        let mut inner = self.inner.lock().expect("error window mutex poisoned");
        match inner.back_mut() {
            Some((m, total, errors)) if *m == minute => {
                *total += 1;
                if !(200..300).contains(&status) {
                    *errors += 1;
                }
            }
            _ => {
                let errors = i64::from(!(200..300).contains(&status));
                inner.push_back((minute, 1, errors));
            }
        }
    }

    /// (total, errors) over the last `window_minutes`, pruning old buckets.
    pub(crate) fn counts(&self, window_minutes: i64) -> (i64, i64) {
        self.counts_at(now_minute(), window_minutes)
    }

    /// Test-visible core: counts for an explicit now-minute.
    pub(crate) fn counts_at(&self, now_minute: i64, window_minutes: i64) -> (i64, i64) {
        let mut inner = self.inner.lock().expect("error window mutex poisoned");
        let cutoff = now_minute - window_minutes;
        while inner.front().is_some_and(|(m, _, _)| *m < cutoff) {
            inner.pop_front();
        }
        let (mut total, mut errors) = (0, 0);
        for (_, t, e) in inner.iter() {
            total += t;
            errors += e;
        }
        (total, errors)
    }
}

/// Current epoch minute (UTC).
pub(crate) fn now_minute() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64 / 60)
        .unwrap_or(0)
}

/// "YYYY-MM-DD HH:MM:SS" in UTC (same shape the old SQLite `datetime('now')`
/// produced), computed without an external date crate.
fn utc_now_str() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant): days since 1970-01-01 → Y-M-D.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

// --- The funnel ------------------------------------------------------------

/// Bound on the usage-writer channel. A full channel logs error! + counts a
/// drop (the audit line already landed in the log stream; only a rollup cell
/// undercounts — loudly).
const USAGE_CHANNEL_CAP: usize = 1024;

/// One usage_daily accumulation unit (built from LogFields in `emit`).
struct UsageDelta {
    service: String,
    provider_used: String,
    key_id: i64,
    token_name: String,
    success: bool,
    tokens: i64,
    cost: f64,
}

/// Everything the request funnel touches that must outlive one request: the
/// ring (admin browse), the error window (alerting), and the usage-writer
/// channel (write-time usage_daily rollup).
#[derive(Clone)]
pub struct RequestEvents {
    pub(crate) ring: Arc<RequestRing>,
    pub(crate) error_window: Arc<ErrorWindow>,
    usage_tx: mpsc::Sender<UsageDelta>,
    writer_stop: Arc<Notify>,
}

impl RequestEvents {
    /// Spawn the single usage writer. The returned handle must be awaited
    /// (with a timeout) after [`RequestEvents::shutdown`] during graceful
    /// shutdown so pending rollup deltas flush before process exit.
    pub fn new(db: Db) -> (RequestEvents, tokio::task::JoinHandle<()>) {
        let (usage_tx, mut usage_rx) = mpsc::channel::<UsageDelta>(USAGE_CHANNEL_CAP);
        let writer_stop = Arc::new(Notify::new());
        let stop = writer_stop.clone();
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    // Graceful shutdown: drain whatever is queued, then stop.
                    _ = stop.notified() => {
                        while let Ok(delta) = usage_rx.try_recv() {
                            upsert_usage(&db, delta).await;
                        }
                        break;
                    }
                    delta = usage_rx.recv() => {
                        let Some(delta) = delta else { break };
                        upsert_usage(&db, delta).await;
                    }
                }
            }
        });
        (
            Self {
                ring: Arc::new(RequestRing::new()),
                error_window: Arc::new(ErrorWindow::new()),
                usage_tx,
                writer_stop,
            },
            writer,
        )
    }

    /// Stop the usage writer after draining queued deltas (graceful shutdown).
    pub fn shutdown(&self) {
        self.writer_stop.notify_one();
    }

    /// Best-effort send; a full channel logs + counts (never blocks the
    /// request path).
    fn send_usage(&self, delta: UsageDelta) {
        match self.usage_tx.try_send(delta) {
            Ok(()) => {}
            Err(_) => {
                tracing::error!("usage channel full; usage_daily delta dropped");
                crate::metrics::record_drop("channel_full");
            }
        }
    }

    /// Integration-test seeding: push an event through the ring + window
    /// without a real HTTP request (deterministic pagination/filter tests).
    #[doc(hidden)]
    pub fn test_push(&self, fields: LogFields) {
        self.ring.push(fields.clone());
        self.error_window.record(fields.status);
    }
}

/// One writer-side upsert with loud failure accounting.
async fn upsert_usage(db: &Db, delta: UsageDelta) {
    let (successes, errors) = if delta.success { (1, 0) } else { (0, 1) };
    if let Err(e) = db
        .upsert_usage_daily(
            &delta.service,
            &delta.provider_used,
            delta.key_id,
            &delta.token_name,
            1,
            successes,
            errors,
            delta.tokens,
            delta.cost,
        )
        .await
    {
        tracing::error!(error = %e, "usage_daily upsert failed");
        crate::metrics::record_drop("upsert_failed");
    }
}

/// Build the usage_daily delta for an event. Every event counts (the old
/// rollup counted 401/validation rows as unknown-service errors too): service
/// falls back to "unknown", provider to "unknown", key/token to the sentinels,
/// tokens/cost to 0.
fn usage_delta(fields: &LogFields) -> UsageDelta {
    UsageDelta {
        service: fields.service.clone().unwrap_or_else(|| "unknown".into()),
        provider_used: fields
            .provider_used
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        key_id: fields.key_id.unwrap_or(0),
        token_name: fields.token_name.clone().unwrap_or_default(),
        success: (200..300).contains(&fields.status),
        tokens: fields.total_tokens.unwrap_or(0),
        cost: fields.cost_est.unwrap_or(0.0),
    }
}

/// Record one finished product request. Synchronous and non-blocking — never
/// fails the request path. Side effects: structured log line, ring entry,
/// error-window update, metrics observation, usage-delta send.
pub fn emit(events: &RequestEvents, fields: LogFields, started: Instant) {
    let duration = started.elapsed();
    let duration_ms = duration.as_millis() as i64;
    let mut fields = fields;
    fields.duration_ms = Some(duration_ms);
    // 1. The durable audit line (stdout JSON logs; retention is owned by the
    //    container log pipeline, not the app).
    tracing::info!(
        target: "request",
        path = fields.path,
        method = "POST",
        status = fields.status,
        duration_ms,
        service = fields.service.as_deref(),
        provider_used = fields.provider_used.as_deref(),
        error_kind = fields.error_kind,
        query_preview = fields.query_preview.as_deref(),
        request_id = fields.request_id.as_deref(),
        token_name = fields.token_name.as_deref(),
        strategy = fields.strategy.as_deref(),
        providers_consulted = fields.providers_consulted.as_deref(),
        attempt_count = fields.attempt_count,
        key_id = fields.key_id,
        node_id = fields.node_id,
        input_tokens = fields.input_tokens,
        output_tokens = fields.output_tokens,
        total_tokens = fields.total_tokens,
        cost_est = fields.cost_est,
        cache_hit = fields.cache_hit,
        "request",
    );
    // 2. Admin browser (bounded in-memory window).
    events.ring.push(fields.clone());
    // 3. Error-rate alert window.
    events.error_window.record(fields.status);
    // 4. Live metrics.
    crate::metrics::observe(
        fields.status,
        fields.service.as_deref(),
        duration,
        fields.input_tokens,
        fields.output_tokens,
        fields.cache_hit,
    );
    // 5. Write-time usage rollup (best-effort; the audit line above already
    //    landed in the log stream — a dropped delta only undercounts a cell).
    events.send_usage(usage_delta(&fields));
}

// --- F08: failed-auth logging ----------------------------------------------

/// API-token extractor that LOGS failed authentication (F08).
///
/// Identical semantics to `crate::ApiToken` (parts-level `FromRequestParts`,
/// so auth still wins over body parsing, F01) but on a rejected token it
/// emits a 401 event before returning the 401 — otherwise failed auth
/// attempts (missing/invalid token) are invisible in the admin surface.
/// `token_name` stays `None`: the token either does not exist or is not
/// present, so there is no name to attribute.
pub struct ApiTokenLogged(pub serpotter_db::TokenRow);

/// Map a request URI to the static path label stored in the event.
fn static_product_path(uri_path: &str) -> &'static str {
    match uri_path {
        "/api/search" => "/api/search",
        "/api/extract" => "/api/extract",
        "/api/research" => "/api/research",
        _ => "/api",
    }
}

/// Build the F08 auth-failure event (401; body never parsed so no preview,
/// no token name, no usage/cost — the request never reached a provider).
fn auth_failure_fields(parts: &Parts) -> LogFields {
    LogFields {
        path: static_product_path(parts.uri.path()),
        status: 401,
        duration_ms: None,
        service: None,
        provider_used: None,
        error_kind: Some("Unauthorized"),
        query_preview: None,
        request_id: request_id_from_headers(&parts.headers),
        token_name: None,
        strategy: None,
        providers_consulted: None,
        attempt_count: None,
        key_id: None,
        node_id: None,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cost_est: None,
        cache_hit: false,
    }
}

#[allow(clippy::result_large_err)]
impl axum::extract::FromRequestParts<AppState> for ApiTokenLogged {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match crate::require_api_token(state, &parts.headers).await {
            Ok(row) => Ok(ApiTokenLogged(row)),
            Err(rejection) => {
                // F08: failed auth emits an event — status 401, request_id
                // from the inbound header (post SetRequestId), path from the
                // URI; the body was never parsed so no preview.
                emit(&state.events, auth_failure_fields(parts), Instant::now());
                Err(rejection)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(status: i64, request_id: &str, token_name: &str, path: &'static str) -> LogFields {
        LogFields {
            path,
            status,
            duration_ms: Some(5),
            service: Some("tavily".into()),
            provider_used: Some("tavily".into()),
            error_kind: None,
            query_preview: Some("q".into()),
            request_id: Some(request_id.into()),
            token_name: Some(token_name.into()),
            strategy: Some("fast".into()),
            providers_consulted: Some("tavily".into()),
            attempt_count: Some(1),
            key_id: Some(1),
            node_id: None,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
            cost_est: None,
            cache_hit: false,
        }
    }

    #[test]
    fn ring_cap_evicts_oldest() {
        let ring = RequestRing::new();
        for i in 0..(RING_CAP + 10) as i64 {
            ring.push(fields(200, &format!("req-{i}"), "t", "/api/search"));
        }
        assert_eq!(ring.len(), RING_CAP);
        // Newest first: the evicted page-0 rows are gone.
        let views = ring.list(&RingFilter {
            limit: 5,
            ..Default::default()
        });
        assert_eq!(views.len(), 5);
        assert_eq!(views[0].id, (RING_CAP + 10) as i64, "newest seq first");
    }

    #[test]
    fn ring_list_filters_and_pages() {
        let ring = RequestRing::new();
        for i in 0..5 {
            ring.push(fields(200, &format!("page-{i}"), "tok-a", "/api/search"));
        }
        for i in 0..3 {
            ring.push(fields(502, &format!("err-{i}"), "tok-b", "/api/extract"));
        }
        // Newest-first page of 2 → err-2, err-1.
        let v = ring.list(&RingFilter {
            limit: 2,
            ..Default::default()
        });
        assert_eq!(v[0].fields.request_id.as_deref(), Some("err-2"));
        assert_eq!(v[1].fields.request_id.as_deref(), Some("err-1"));
        // Offset pages into the older half.
        let v = ring.list(&RingFilter {
            limit: 2,
            offset: 2,
            ..Default::default()
        });
        assert_eq!(v[0].fields.request_id.as_deref(), Some("err-0"));
        assert_eq!(v[1].fields.request_id.as_deref(), Some("page-4"));
        // token_name exact.
        let v = ring.list(&RingFilter {
            limit: 10,
            token_name: Some("tok-a".into()),
            ..Default::default()
        });
        assert_eq!(v.len(), 5);
        assert!(v
            .iter()
            .all(|r| r.fields.token_name.as_deref() == Some("tok-a")));
        // path prefix.
        let v = ring.list(&RingFilter {
            limit: 10,
            path_prefix: Some("/api/extract".into()),
            ..Default::default()
        });
        assert_eq!(v.len(), 3);
        // status exact.
        let v = ring.list(&RingFilter {
            limit: 10,
            status: Some(502),
            ..Default::default()
        });
        assert_eq!(v.len(), 3);
        // request_id exact.
        let v = ring.list(&RingFilter {
            limit: 10,
            request_id: Some("page-2".into()),
            ..Default::default()
        });
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn error_window_buckets_prunes_and_counts() {
        let w = ErrorWindow::new();
        // 20 errors at minute 100, 10 ok at minute 101.
        for _ in 0..20 {
            w.record_at(500, 100);
        }
        for _ in 0..10 {
            w.record_at(200, 101);
        }
        // Window 5 from minute 103: all buckets (100..=101 >= 98) count.
        let (total, errors) = w.counts_at(103, 5);
        assert_eq!(total, 30);
        assert_eq!(errors, 20);
        // Window 1 from minute 102: cutoff 101 prunes minute 100, so only
        // minute 101 (the newest bucket) survives.
        let (total, errors) = w.counts_at(102, 1);
        assert_eq!(total, 10);
        assert_eq!(errors, 0);
        // Old buckets pruned when counted past the cutoff.
        let (total, _) = w.counts_at(200, 5);
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn emit_feeds_ring_error_window_and_usage_channel() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let (events, _writer) = RequestEvents::new(db);
        emit(
            &events,
            fields(502, "req-1", "t", "/api/search"),
            Instant::now(),
        );
        assert_eq!(events.ring.len(), 1);
        let (total, errors) = events.error_window.counts(5);
        assert_eq!((total, errors), (1, 1));
        // The usage delta is queued (writer picks it up asynchronously).
        events.shutdown();
        // The writer handle was detached in the test; give it a beat to flush.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn usage_writer_flushes_queued_deltas_on_shutdown() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let (events, writer) = RequestEvents::new(db.clone());
        let k = db.insert_api_key("tavily", "tvly-w").await.unwrap();
        events.send_usage(UsageDelta {
            service: "tavily".into(),
            provider_used: "tavily".into(),
            key_id: k.id,
            token_name: "tok-w".into(),
            success: true,
            tokens: 100,
            cost: 1.5,
        });
        events.shutdown();
        // JoinHandle<()>: the writer completing cleanly IS the flush proof.
        tokio::time::timeout(std::time::Duration::from_secs(2), writer)
            .await
            .expect("writer must stop after shutdown")
            .expect("writer task must not panic");
        let rows = db.usage_summary(7).await.expect("usage summary");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tokens, 100);
        assert!((rows[0].cost - 1.5).abs() < 1e-9);
    }

    #[test]
    fn utc_now_str_shape() {
        let s = utc_now_str();
        assert_eq!(s.len(), 19, "YYYY-MM-DD HH:MM:SS: {s}");
        assert_eq!(s.as_bytes()[4], b'-');
        assert_eq!(s.as_bytes()[10], b' ');
    }

    #[test]
    fn hybrid_dial_uses_first_consulted_as_service() {
        let mut meta = ExecMeta::default();
        meta.note_attempt("tavily", 1, None, true);
        meta.note_attempt("firecrawl", 2, None, false);
        assert_eq!(
            service_from_meta(Some("hybrid"), &meta).as_deref(),
            Some("tavily")
        );
    }

    #[test]
    fn single_provider_service_matches_dial() {
        let mut meta = ExecMeta::default();
        meta.note_attempt("exa", 3, Some(9), true);
        assert_eq!(
            service_from_meta(Some("exa"), &meta).as_deref(),
            Some("exa")
        );
    }

    #[test]
    fn error_with_attempts_uses_last_consulted() {
        let mut meta = ExecMeta::default();
        meta.note_attempt("tavily", 1, None, false);
        meta.note_attempt("firecrawl", 2, None, false);
        assert_eq!(service_from_meta(None, &meta).as_deref(), Some("firecrawl"));
    }

    #[test]
    fn research_dial_verify_maps_to_blend_verify() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("verify".into());
        meta.note_attempt("tavily", 1, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("blend-verify"));
        // strategy column stays raw when fields_from_meta is used
        let f = fields_from_meta(
            "/api/research",
            200,
            None,
            None,
            None,
            None,
            research_dial_label(&meta),
            &meta,
        );
        assert_eq!(f.provider_used.as_deref(), Some("blend-verify"));
        assert_eq!(f.strategy.as_deref(), Some("verify"));
        assert_eq!(f.service.as_deref(), Some("tavily"));
    }

    #[test]
    fn research_dial_balanced_maps_to_blend() {
        // F16: strategy stores the raw routed strategy ("balanced" for a
        // 2-leg blend); the research dial label derives "blend" from it.
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, None, true);
        meta.note_attempt("firecrawl", 2, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("blend"));
    }

    #[test]
    fn research_dial_fast_uses_first_vendor() {
        // F16: a fast single-chain web leg (raw strategy "fast") maps to the
        // first consulted vendor, not the raw strategy string.
        let mut meta = ExecMeta::default();
        meta.strategy = Some("fast".into());
        meta.note_attempt("tavily", 1, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("tavily"));
    }

    #[test]
    fn research_dial_single_uses_first_vendor() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("single".into());
        meta.note_attempt("exa", 3, None, true);
        assert_eq!(research_dial_label(&meta).as_deref(), Some("exa"));
    }

    // ---- F60/D9: success-path fields_from_meta mapping with a REAL ExecMeta ----

    /// A single successful provider attempt: every log field maps from meta.
    #[test]
    fn fields_from_meta_success_single_provider() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("fast".into());
        meta.note_attempt("tavily", 42, Some(7), true);
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            Some("hello world".into()),
            Some("req-123".into()),
            Some("tok-a".into()),
            Some("tavily".into()),
            &meta,
        );
        assert_eq!(f.service.as_deref(), Some("tavily"));
        assert_eq!(f.provider_used.as_deref(), Some("tavily"));
        assert_eq!(f.strategy.as_deref(), Some("fast"));
        assert_eq!(f.providers_consulted.as_deref(), Some("tavily"));
        assert_eq!(f.attempt_count, Some(1));
        assert_eq!(f.key_id, Some(42));
        assert_eq!(f.node_id, Some(7));
        assert_eq!(f.status, 200);
        assert_eq!(f.error_kind, None);
    }

    /// Multi-leg success: sticky LAST success key/node wins; providers and
    /// attempts accumulate first-seen.
    #[test]
    fn fields_from_meta_success_multi_leg_sticky_last() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, Some(10), false);
        meta.note_attempt("firecrawl", 2, Some(11), true);
        meta.note_attempt("exa", 3, Some(12), true);
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            None,
            Some("req-456".into()),
            Some("tok-b".into()),
            Some("blend".into()),
            &meta,
        );
        // provider_used is a dial label → service = first consulted real vendor.
        assert_eq!(f.service.as_deref(), Some("tavily"));
        assert_eq!(f.provider_used.as_deref(), Some("blend"));
        assert_eq!(
            f.providers_consulted.as_deref(),
            Some("tavily,firecrawl,exa")
        );
        assert_eq!(f.attempt_count, Some(3));
        // sticky last success = exa's hold, not the failed tavily attempt.
        assert_eq!(f.key_id, Some(3));
        assert_eq!(f.node_id, Some(12));
        assert_eq!(f.strategy.as_deref(), Some("balanced"));
    }

    /// Hybrid with a x-leg success: first-seen order and last-success key.
    #[test]
    fn fields_from_meta_hybrid_web_then_x() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, None, true);
        meta.note_attempt("xai", 9, None, true);
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            None,
            None,
            None,
            Some("hybrid".into()),
            &meta,
        );
        assert_eq!(f.service.as_deref(), Some("tavily"));
        assert_eq!(f.providers_consulted.as_deref(), Some("tavily,xai"));
        // sticky LAST success is the x leg.
        assert_eq!(f.key_id, Some(9));
        assert!(f.node_id.is_none());
    }

    /// B2: usage/cost from ExecMeta flow into the log fields; the B1 cache
    /// serve flag threads to the metrics counter.
    #[test]
    fn fields_from_meta_maps_usage_cost_and_cache() {
        let mut meta = ExecMeta::default();
        meta.strategy = Some("balanced".into());
        meta.note_attempt("tavily", 1, None, true);
        meta.input_tokens = Some(120);
        meta.output_tokens = Some(80);
        meta.total_tokens = Some(200);
        meta.cost = Some(0.0042);
        meta.cache_hit = true;
        let f = fields_from_meta(
            "/api/search",
            200,
            None,
            None,
            Some("req-xyz".into()),
            Some("tok-c".into()),
            Some("tavily".into()),
            &meta,
        );
        assert_eq!(f.input_tokens, Some(120));
        assert_eq!(f.output_tokens, Some(80));
        assert_eq!(f.total_tokens, Some(200));
        assert_eq!(f.cost_est, Some(0.0042));
        assert!(f.cache_hit, "B1 serve flag threads to the metrics counter");
    }

    #[test]
    fn fields_from_meta_default_meta_is_null_usage() {
        let meta = ExecMeta::default();
        let f = fields_from_meta(
            "/api/extract",
            200,
            None,
            None,
            None,
            None,
            Some("tavily".into()),
            &meta,
        );
        assert!(f.input_tokens.is_none());
        assert!(f.output_tokens.is_none());
        assert!(f.total_tokens.is_none());
        assert!(f.cost_est.is_none());
        assert!(!f.cache_hit, "default meta never served from cache");
    }

    /// F08 401 rows never carry provider usage (the request never reached a
    /// provider).
    #[test]
    fn auth_failure_fields_have_null_cost() {
        // http::request::Parts has no Default — build one from a real request.
        let (parts, _) = axum::http::Request::builder()
            .uri("/api/search")
            .header("x-request-id", "req-401")
            .body(())
            .expect("build request")
            .into_parts();
        let f = auth_failure_fields(&parts);
        assert_eq!(f.status, 401);
        assert_eq!(f.path, "/api/search");
        assert_eq!(f.request_id.as_deref(), Some("req-401"));
        assert_eq!(f.token_name, None);
        assert_eq!(f.error_kind, Some("Unauthorized"));
        assert!(f.input_tokens.is_none() && f.output_tokens.is_none());
        assert!(f.total_tokens.is_none() && f.cost_est.is_none());
        assert!(!f.cache_hit, "an auth failure never served from cache");
    }

    #[test]
    fn error_before_vendor_service_none() {
        let meta = ExecMeta::default();
        assert!(service_from_meta(None, &meta).is_none());
    }
}
