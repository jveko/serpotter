//! Event-funnel coverage for the paths that previously emitted nothing:
//! body-extractor rejections on the product routes, the F10 504 kind and its
//! attribution, the MCP auth failure (P2-5), and the DatabaseError problem.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::Value;

/// Serializes every test in this file that reads or writes the process-global
/// METRICS registry, exactly like the in-crate `METRICS_LOCK`.
///
/// `serpotter_api::metrics_*_count` are process-global `IntCounterVec`s and
/// these tests assert on ABSOLUTE deltas over the `("tavily", …)` label pairs.
/// Two such tests running in parallel on different threads would interleave:
/// the 401 ladder's `auth_invalid`/`disabled` increments could land inside the
/// healthy-search test's snapshot→request→assert window and be counted as its
/// own, making the green run unreproducible. The lock makes each window
/// exclusive; it is never held across an unrelated await.
static METRICS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Mock upstream that accepts connections and never answers, so the product
/// call is parked inside a real provider attempt when the deadline fires.
/// Returns its base URL.
fn spawn_blackhole() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind blackhole");
    let addr = listener.local_addr().expect("blackhole addr");
    std::thread::spawn(move || {
        // Hold every accepted socket open and never answer: the client's
        // request hangs until the request deadline drops the future.
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            held.push(stream);
        }
    });
    format!("http://{addr}")
}

/// App state whose Tavily client dials `url` (everything else stays on the
/// dead 127.0.0.1:9 pin).
fn state_with_tavily(
    db: serpotter_db::Db,
    url: String,
    request_timeout: Duration,
) -> serpotter_api::AppState {
    let mut st = state_with(db);
    st.providers = serpotter_providers::ProviderRegistry::with_clients(
        serpotter_providers::TavilyClient::new(url),
        serpotter_providers::FirecrawlClient::new("http://127.0.0.1:9"),
        serpotter_providers::ExaClient::new("http://127.0.0.1:9"),
        serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
    );
    st.product_config = st.product_config.with_request_timeout(request_timeout);
    st
}

/// Poll the admin request-log for the row carrying `request_id`.
async fn log_row(app: axum::Router, request_id: &str) -> Value {
    for _ in 0..100 {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?limit=100")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "admin list must authorize");
        let v = body_json(res).await;
        if let Some(row) = v
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["requestId"] == request_id))
        {
            return row.clone();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no request-log row for requestId {request_id}");
}

async fn post_json(app: &axum::Router, uri: &str, body: &'static str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("x-request-id", REQ_ID)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

const REQ_ID: &str = "events-req-1";

// --- 1. boundary rejections now reach the funnel -----------------------------

/// A malformed body is exactly the request an operator needs to see, and it
/// used to leave no ring row, no error-window bucket and no metric: the
/// `AppJson` rejection answered problem+json straight from the extractor.
/// The 400 event must carry the same kind the wire problem does, AND reach
/// all three sinks of the funnel — a refactor that emitted straight to the
/// ring would otherwise stay green while the alert window and the metrics
/// silently went blind.
#[tokio::test]
async fn malformed_body_emits_an_event() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let st = state_with(db);
    // Baseline on this state's own funnel: the admin log poll below issues
    // requests too, so the window is read as a DELTA.
    let events = st.events.clone();
    let (t0, e0) = events.test_error_window_counts(5);
    // Metrics baseline: these counters are PROCESS-GLOBAL, and sibling tests
    // in this binary already produce ("unknown", "error") rows — every 401
    // auth failure has a null service. An absolute `>= 1` would therefore be
    // satisfied without this request ever reaching `metrics::observe`, so the
    // assertion has to be a DELTA.
    let m0 = serpotter_api::metrics_requests_count("unknown", "error");
    let app = app(st);
    let res = post_json(&app, "/api/search", r#"{"query":"#).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(v["type"].as_str().unwrap_or("").ends_with("/InvalidJson"));

    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["path"], "/api/search", "row: {row}");
    assert_eq!(row["status"], 400);
    assert_eq!(
        row["errorKind"], "InvalidJson",
        "event kind must match the wire kind: {row}"
    );
    assert_eq!(row["tokenName"], "t", "auth passed: {row}");
    assert!(
        row["service"].is_null() && row["providersConsulted"].is_null(),
        "a body rejection never reached a vendor: {row}"
    );

    // The ERROR-WINDOW side: a 400 is an error, so it must raise the
    // high-error-rate alert's `(total, errors)` — not just the total.
    let (t1, e1) = events.test_error_window_counts(5);
    assert!(
        t1 > t0 && e1 > e0,
        "a 400 rejection must reach the alert window: total {t0}->{t1}, errors {e0}->{e1}"
    );
    // The METRICS side: `events::emit` labels a service-less rejection
    // "unknown", and a 400 is the `error` status class. Delta, not an
    // absolute count (see the baseline above).
    let m1 = serpotter_api::metrics_requests_count("unknown", "error");
    assert!(m1 > m0, "a 400 rejection must reach /metrics: {m0} -> {m1}");
}

/// The over-limit body is the fourth `AppJson` rejection and the only one
/// whose status is produced by the body-limit layer rather than the JSON
/// parser; it must still land a row naming `BodyTooLarge` (not `InvalidJson`),
/// or an operator sees a size problem filed as a syntax problem.
#[tokio::test]
async fn oversized_body_emits_413_event() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let payload = format!(
        r#"{{"query":"{}"}}"#,
        "k".repeat(serpotter_api::BODY_LIMIT_BYTES + 128 * 1024)
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("x-request-id", REQ_ID)
                .header("content-length", payload.len().to_string())
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["status"], 413, "row: {row}");
    assert_eq!(
        row["errorKind"], "BodyTooLarge",
        "a size rejection must not be filed as a JSON syntax error: {row}"
    );
}

/// A wrong-shape body (422) and a missing content-type (415) are the other two
/// `AppJson` rejections, and each must name its own kind rather than a
/// catch-all `InvalidJson`.
#[tokio::test]
async fn wrong_shape_body_emits_422_event() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = post_json(&app, "/api/extract", "{}").await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["path"], "/api/extract", "row: {row}");
    assert_eq!(row["status"], 422);
    assert_eq!(row["errorKind"], "InvalidJson");
}

#[tokio::test]
async fn missing_content_type_emits_415_event() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("x-request-id", REQ_ID)
                .body(Body::from(r#"{"query":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["path"], "/api/research", "row: {row}");
    assert_eq!(row["status"], 415);
    assert_eq!(
        row["errorKind"], "InvalidContentType",
        "415 must not be filed as a JSON syntax error: {row}"
    );
}

/// An unauthenticated malformed body still answers 401 (auth wins over body
/// parsing) and must produce exactly ONE event: the 401 from the auth
/// extractor, never a second body-rejection row for the same request.
#[tokio::test]
async fn unauthenticated_malformed_body_logs_one_401_event() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("content-type", "application/json")
                .header("x-request-id", REQ_ID)
                .body(Body::from(r#"{"query":"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let row = log_row(app, REQ_ID).await;
    assert_eq!(
        row["status"], 401,
        "auth wins, and is the only event: {row}"
    );
    assert_eq!(row["errorKind"], "Unauthorized");
}

// --- 2/3. the 504 event: kind parity and real attribution --------------------

/// The F10 504 is the one case where the product's `ExecMeta` is never
/// returned, so the event used to say `service: null, attemptCount: 0` for a
/// request that had already leased a key and dialed a vendor. This is the
/// end-to-end proof that the product-side sink is actually observed by the
/// future the handler built: the blackhole parks the call INSIDE the tavily
/// attempt, and the row must name it.
#[tokio::test]
async fn timeout_event_carries_the_vendor_it_was_dialing() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tv-timeout").await.unwrap();
    let st = state_with_tavily(db, spawn_blackhole(), Duration::from_secs(1));
    let app = app(st);
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
    )
    .await;
    assert_eq!(
        res.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "the blackhole must outlast the 1s deadline"
    );
    let v = body_json(res).await;
    assert!(
        v["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/RequestTimeout"),
        "wire kind: {v}"
    );

    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["status"], 504, "row: {row}");
    assert_eq!(
        row["errorKind"], "RequestTimeout",
        "the event kind must match the wire kind, not the MCP-only \"Timeout\": {row}"
    );
    assert_eq!(
        row["service"], "tavily",
        "a 504 must name the vendor it was actually on: {row}"
    );
    assert!(
        !row["keyId"].is_null(),
        "the leased key must be attributed: {row}"
    );
}

/// The `ExtractTimeout` distinction the audit calls for: a vendor job that
/// outlives its own poll window keeps its own kind, while the F10 request
/// deadline answers `RequestTimeout`. This pins that the REST 504 arm no
/// longer emits the MCP-flavoured `Timeout` that made the two unfilterable.
#[tokio::test]
async fn request_deadline_kind_differs_from_the_mcp_timeout_kind() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tv-timeout-2").await.unwrap();
    let st = state_with_tavily(db, spawn_blackhole(), Duration::from_secs(1));
    let app = app(st);
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
    )
    .await;
    assert_eq!(res.status(), StatusCode::GATEWAY_TIMEOUT);
    let row = log_row(app, REQ_ID).await;
    assert_ne!(
        row["errorKind"], "Timeout",
        "\"Timeout\" is the MCP tool kind; REST 504 rows are filterable as RequestTimeout"
    );
}

// --- 4. MCP auth failures are visible (P2-5) ---------------------------------

/// REST's `ApiTokenLogged` emits a 401 event (F08); the MCP transport used to
/// answer the same 401 in silence, so a client hammering `/mcp` with a bad
/// token produced no ring row at all.
#[tokio::test]
async fn mcp_401_emits_an_event() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Authorization", "Bearer tok-does-not-exist")
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("x-request-id", REQ_ID)
                .body(Body::from(MCP_INIT_BODY))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["status"], 401, "row: {row}");
    assert_eq!(row["errorKind"], "Unauthorized");
    assert_eq!(
        row["path"], "/mcp",
        "an MCP auth failure must not be filed under the REST catch-all: {row}"
    );
    assert!(row["tokenName"].is_null(), "no such token to name: {row}");
}

/// A VALID token must not produce an auth-failure row: the middleware emits
/// only on the rejected branch.
#[tokio::test]
async fn mcp_valid_token_emits_no_auth_failure_event() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .header("accept", MCP_ACCEPT)
                .header("x-request-id", REQ_ID)
                .body(Body::from(MCP_INIT_BODY))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED, "auth must pass");

    let listed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?limit=100")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v = body_json(listed).await;
    assert!(
        !v.as_array().is_some_and(|rows| rows
            .iter()
            .any(|r| r["requestId"] == REQ_ID && r["errorKind"] == "Unauthorized")),
        "a valid token must not log an auth failure"
    );
}

// --- 5. DatabaseError: generic detail, not retryable --------------------------

/// Closing the pool makes every key acquire fail with a REAL
/// `DbError` (a closed pool), which the product layer maps to
/// `DatabaseError`. The wire must answer a fixed detail — never the driver's
/// text, which for a closed-pool failure names the pool internals — and must
/// mark the fault `retryable:false` so a client does not hammer a broken
/// database. The same row is still auditable through the event funnel.
#[tokio::test]
async fn database_error_problem_is_generic_and_not_retryable() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // The fault is injected on the OUTBOUND pool's own Db handle, never on
    // `state.db`: the auth extractor's token lookup shares `state.db`, so
    // closing that one would answer a different 500 (`Token lookup failed`)
    // before the product call ever runs.
    //
    // A key is seeded for EVERY vendor in the chain. The key lease succeeds
    // (it uses the healthy pool) and the very next acquire — the outbound one
    // — fails with a real `DbError`, which the product maps to
    // `DatabaseError`. A keyless leg instead fails as `NoHealthyKey`, and the
    // chain's last-leg-wins rule lets that 503 mask the storage fault — an
    // availability question, not what this test is about.
    for service in ["tavily", "exa", "firecrawl"] {
        db.insert_api_key(service, &format!("sk-db-fault-{service}"))
            .await
            .unwrap();
    }
    let outbound_db = test_db().await;
    outbound_db.pool().close().await;

    let mut st = state_with_require_proxy(db);
    st.outbound = Arc::new(serpotter_outbound::ProxyPool::with_options(
        outbound_db,
        true,
    ));
    let app = app(st);
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"firecrawl","strategy":"fast"}"#,
    )
    .await;
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let status = res.status();
    let problem: Value = body_json(res).await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a closed pool is our fault, not the vendor's: {problem}"
    );
    assert_eq!(
        content_type.as_deref().and_then(|v| v.split(';').next()),
        Some("application/problem+json"),
        "problem+json: {problem}"
    );
    assert!(
        problem["type"]
            .as_str()
            .unwrap_or("")
            .ends_with("/DatabaseError"),
        "kind: {problem}"
    );
    let detail = problem["detail"].as_str().unwrap_or("").to_lowercase();
    assert!(
        !detail.contains("sqlite") && !detail.contains("pool") && !detail.contains("select"),
        "the driver text must never reach the wire: {problem}"
    );
    assert_eq!(
        problem["retryable"], false,
        "a storage fault is not something a client should retry: {problem}"
    );

    // The event funnel still records the real classification.
    let row = log_row(app, REQ_ID).await;
    assert_eq!(row["status"], 500, "row: {row}");
    assert_eq!(row["errorKind"], "DatabaseError");
}

// --- 6. vendor-level failures are countable per attempt ----------------------

/// A request-level row answers "did my API request fail"; it cannot answer
/// "which vendor keys are being rejected", because one request may absorb
/// several attempts and the row collapses them into a single status. The
/// scripted upstream answers 401 on every dial, so the product classifies
/// the attempt `auth_invalid` — and the event must still count it, even
/// though the request's own row reads as a vendor error.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // METRICS_LOCK: same ("tavily","auth_invalid") pair as test 7
async fn classified_provider_failure_is_counted_per_attempt() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tv-attempt-metric")
        .await
        .unwrap();
    let st = state_with_tavily(
        db,
        spawn_scripted(401, r#"{"error":"invalid api key"}"#),
        Duration::from_secs(10),
    );
    let app = app(st);
    let _guard = METRICS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let before = serpotter_api::metrics_attempt_count("tavily", "auth_invalid");
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
    )
    .await;
    assert!(
        !res.status().is_success(),
        "a 401 on every leg must not be answered as a success: {:?}",
        res.status()
    );

    assert!(
        serpotter_api::metrics_attempt_count("tavily", "auth_invalid") > before,
        "the 401 attempt must be counted as auth_invalid, not lost behind the request row"
    );
}

// --- 7. key-state transitions are countable, not just visible in the row ------

/// The 401 ladder's third attempt is what flips `active = 0` and stamps
/// `auth_fail`, and that flip is invisible in the request row: the request
/// simply failed. A request-level counter therefore cannot answer "is this
/// vendor taking my keys out of rotation?" — the pool-depth gauge only shows it
/// on the next cron tick. This is the end-to-end proof that the post-state the
/// db report returns reaches the metrics sink.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // METRICS_LOCK makes the delta window exclusive
async fn key_disabled_by_the_401_ladder_is_counted_as_a_transition() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let k = db
        .insert_api_key("tavily", "tvly-transition")
        .await
        .unwrap();
    let st = state_with_tavily(
        db.clone(),
        spawn_scripted(401, r#"{"error":"invalid api key"}"#),
        Duration::from_secs(10),
    );
    let app = app(st);
    let _guard = METRICS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let attempts_before = serpotter_api::metrics_attempt_count("tavily", "auth_invalid");
    let disabled_before = serpotter_api::metrics_key_transition_count("tavily", "disabled");
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
    )
    .await;
    assert!(!res.status().is_success(), "a 401 ladder must not succeed");

    // EXACT deltas, not `>=`: the ladder is 3 attempts, and the disable can
    // only happen once (afterwards the key is out of the pool). A loose bound
    // would also be satisfiable by a concurrent test's own ladder, which is
    // what the lock above rules out.
    assert_eq!(
        serpotter_api::metrics_attempt_count("tavily", "auth_invalid"),
        attempts_before + 3,
        "the 401 ladder must run exactly three attempts"
    );
    assert_eq!(
        serpotter_api::metrics_key_transition_count("tavily", "disabled"),
        disabled_before + 1,
        "the third 401 must be counted as exactly one key disable, not lost behind the failed request row"
    );

    // The counter must mean what it says: the row really is out of rotation.
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(
        row.active, 0,
        "a counted disable means the key really left the pool"
    );
    assert_eq!(row.consecutive_fails, 3);
}

/// A request that never trips a threshold must not move the transition
/// counter: a 500 ladder retries, but a retry is not a key state change, and a
/// counter that grew here would be indistinguishable from a disable.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // METRICS_LOCK makes the delta window exclusive
async fn a_healthy_attempt_records_no_transition() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-noop").await.unwrap();
    let st = state_with_tavily(
        db,
        spawn_scripted(
            200,
            r#"{"results":[{"title":"t","url":"https://e.com","content":"c","score":0.9}]}"#,
        ),
        Duration::from_secs(10),
    );
    let app = app(st);
    let _guard = METRICS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let before = serpotter_api::metrics_key_transition_count("tavily", "disabled");
    let attempts_before = serpotter_api::metrics_attempt_count("tavily", "ok");
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
    )
    .await;
    assert!(res.status().is_success(), "{:?}", res.status());
    assert_eq!(
        serpotter_api::metrics_key_transition_count("tavily", "disabled"),
        before,
        "a successful search must not count a key transition"
    );
    // The guard is not vacuous: the request really did reach the metrics sink
    // (as an `ok` attempt). A test that only ever observed an unmoving
    // counter would pass for a request that never emitted at all.
    assert_eq!(
        serpotter_api::metrics_attempt_count("tavily", "ok"),
        attempts_before + 1,
        "the successful leg must still be counted as one ok attempt"
    );
}

// --- 8. the per-attempt detail is queryable, not just countable -----------

/// The 401 ladder is precisely the "why is my key gone?" question, and the
/// request row answered it with a bare failure: no per-attempt outcome, no
/// upstream status, no key that was disabled. Counters (test 7) answer "how
/// often", never "which request, which key" — the row has to carry the
/// evidence itself, or the admin browser can only guess from `errorKind`.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // METRICS_LOCK shares the ("tavily","auth_invalid") window with test 7
async fn failed_attempts_serialize_their_outcomes_and_transitions_into_the_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-row").await.unwrap();
    let st = state_with_tavily(
        db.clone(),
        spawn_scripted(401, r#"{"error":"invalid api key"}"#),
        Duration::from_secs(10),
    );
    // A row that never reached an upstream (transport failure, cache hit, an
    // auth rejection before any dial) carries NO lastUpstreamStatus. Seeded
    // here so the filter's documented exclusion of those rows is observable:
    // with only the ladder's own row in the ring, "excludes rows without a
    // status" and "returns everything" would be the same result.
    st.events.test_push(no_upstream_status_row());
    let app = app(st);
    let _guard = METRICS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let res = post_json(
        &app,
        "/api/search",
        r#"{"query":"hello","provider":"tavily","strategy":"fast"}"#,
    )
    .await;
    assert!(!res.status().is_success(), "a 401 ladder must not succeed");

    let row = log_row(app.clone(), REQ_ID).await;
    let outcomes = row["attemptOutcomes"]
        .as_str()
        .expect("attemptOutcomes must be serialized");
    assert!(
        outcomes.contains("tavily:auth_invalid:401"),
        "each attempt must name its outcome and upstream status, got {outcomes:?}"
    );
    assert_eq!(
        row["lastUpstreamStatus"], 401,
        "the last upstream status the ladder saw must be filterable"
    );
    let key_ids = row["keyIds"].as_str().expect("keyIds must be serialized");
    assert!(
        key_ids.split(',').any(|id| id == k.id.to_string()),
        "the attempted key {id} must be named, got {key_ids:?}",
        id = k.id
    );
    // The disable is the whole point of the row: the counter (test 7) knows a
    // key left the pool, only the row knows WHICH one.
    let transitions = row["keyTransitions"]
        .as_str()
        .expect("a 401 ladder must serialize the disable it caused");
    assert_eq!(
        transitions,
        format!("tavily:disabled:{}", k.id),
        "the transition must name service, label and the transitioned key"
    );

    // The same evidence must be REACHABLE: the row matching lastUpstreamStatus
    // is found by the server-side filter, and a status nobody saw is not.
    let matching = log_rows(app.clone(), "lastUpstreamStatus=401").await;
    assert!(
        matching
            .iter()
            .any(|r| r["requestId"] == REQ_ID && r["keyTransitions"] == transitions),
        "lastUpstreamStatus=401 must return the enriched row, got {matching:?}"
    );
    assert!(
        log_rows(app.clone(), "lastUpstreamStatus=403")
            .await
            .iter()
            .all(|r| r["lastUpstreamStatus"] == 403),
        "a lastUpstreamStatus filter must not leak rows with another upstream status"
    );
    // Rows that never reached an upstream (transport failure, cache hit) carry
    // no lastUpstreamStatus, so a status filter must EXCLUDE them rather than
    // treating "unknown" as a match. The seeded `no-upstream` row is the
    // witness: it is in the ring, and it must not come back filtered.
    let unfiltered = log_rows(app.clone(), "").await;
    let filtered = log_rows(app.clone(), "lastUpstreamStatus=401").await;
    assert!(
        unfiltered.iter().any(|r| r["requestId"] == NO_UPSTREAM_ID),
        "the seeded no-upstream row must be in the ring, else this proves nothing"
    );
    assert!(
        !filtered.iter().any(|r| r["requestId"] == NO_UPSTREAM_ID),
        "a row that never reached an upstream must be excluded while a status filter is set, got {filtered:?}"
    );
    assert!(
        filtered.iter().all(|r| r["lastUpstreamStatus"] == 401),
        "every filtered row must carry the requested upstream status"
    );
    // Lenient like `status`: a class-range a dashboard passes through is
    // treated as absent, never a 400, and absent means "no filtering" — which
    // is exactly what brings the no-upstream row back.
    let bogus = log_rows(app, "lastUpstreamStatus=4xx").await;
    assert_eq!(
        bogus.len(),
        unfiltered.len(),
        "a non-numeric lastUpstreamStatus must be ignored, not an error"
    );
    assert!(
        bogus.iter().any(|r| r["requestId"] == NO_UPSTREAM_ID),
        "an ignored lastUpstreamStatus must not narrow the window at all"
    );
}

/// Request id of the seeded row that never reached an upstream.
const NO_UPSTREAM_ID: &str = "events-req-no-upstream";

/// A row whose request never completed a provider attempt, so it carries no
/// `lastUpstreamStatus` — the shape the `lastUpstreamStatus` filter must skip.
fn no_upstream_status_row() -> serpotter_api::events::LogFields {
    serpotter_api::events::LogFields {
        path: "/api/search",
        status: 200,
        duration_ms: Some(5),
        service: None,
        provider_used: None,
        error_kind: None,
        query_preview: None,
        request_id: Some(NO_UPSTREAM_ID.into()),
        token_name: None,
        strategy: None,
        providers_consulted: None,
        attempt_count: Some(0),
        key_id: None,
        node_id: None,
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cost_est: None,
        cache_hit: true,
        attempt_log: Vec::new(),
        key_transitions: Vec::new(),
        attempt_outcomes: None,
        last_upstream_status: None,
        key_ids: None,
        key_transitions_csv: None,
    }
}

/// Newest-first admin rows, optionally filtered, read straight from the ring.
async fn log_rows(app: axum::Router, extra: &str) -> Vec<Value> {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/request-logs?limit=100&{extra}"))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await.as_array().cloned().unwrap_or_default()
}
