//! Runtime-behavior coverage for the `/mcp` transport, beyond the wire
//! contract (`mcp_stateless.rs`) and the legacy session lifecycle
//! (`mcp_session.rs`):
//!
//! - the deadline/cancel exits flush queued progress frames before returning
//!   (the poll-order half of the race is pinned by the in-crate
//!   `mcp::race_tests`, which can construct a ready-future/already-elapsed
//!   -deadline pair that no HTTP test can)
//! - CORS preflight is answerable without a token, and only for allowlisted
//!   origins
//! - a legacy session id is bound to the token that opened it
//! - the per-token in-flight cap refuses over-cap callers retryably, before
//!   any progress delivery task exists
//! - `health` reports a storage fault as an error envelope AND emits an event

mod common;

use std::time::Duration;

use common::*;
use serde_json::Value;

/// Canonical session header, as the server defines it. Using the exported
/// constant here (rather than the `"mcp-session-id"` literal) is what makes
/// that public API load-bearing: a rename breaks this suite instead of
/// silently passing.
const SESSION_HEADER: &str = serpotter_api::MCP_SESSION_HEADER;

// --- helpers -----------------------------------------------------------------

/// Serializes every test in this binary that mutates process env, so the
/// CORS cases cannot observe each other's allowlist. Async-aware because the
/// guard is held across the whole request.
async fn env_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// Mock upstream that accepts connections and never answers, so a product
/// call parks INSIDE a real provider attempt for the whole test.
fn spawn_blackhole() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind blackhole");
    let addr = listener.local_addr().expect("blackhole addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            held.push(stream);
        }
    });
    format!("http://{addr}")
}

/// App state whose Tavily client dials `url` (everything else stays on the
/// dead 127.0.0.1:9 pin), with a long acquire timeout so admitted calls stay
/// in flight for the duration of a test instead of failing fast at the key
/// pool, and the given overall request deadline.
fn state_parked(db: serpotter_db::Db, url: String) -> serpotter_api::AppState {
    let mut st = state_with_key_pool(
        db,
        /* max_inflight */ 1,
        Duration::from_secs(60),
        serpotter_db::KEY_HOLD_TTL_SECS,
    );
    st.providers = serpotter_providers::ProviderRegistry::with_clients(
        serpotter_providers::TavilyClient::new(url),
        serpotter_providers::FirecrawlClient::new("http://127.0.0.1:9"),
        serpotter_providers::ExaClient::new("http://127.0.0.1:9"),
        serpotter_providers::XaiClient::new("http://127.0.0.1:9"),
    );
    st.product_config = st
        .product_config
        .with_request_timeout(Duration::from_secs(60));
    st
}

/// The error envelope from a tool result's JSON text block. Failures carry it
/// in `content` only (the tools advertise an `outputSchema` the envelope
/// cannot satisfy), so that is where every assertion reads it from.
fn error_envelope(result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("text block present: {result}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("text block is JSON ({e}): {text}"))
}

/// A `tools/call` for `name` with the given query, optionally asking for
/// progress under `progress`.
fn tool_call(name: &str, query: &str, progress: Option<&str>) -> Request<Body> {
    let mut params = serde_json::Map::new();
    params.insert("name".into(), Value::String(name.into()));
    let mut arguments = serde_json::Map::new();
    if !query.is_empty() {
        arguments.insert("query".into(), Value::String(query.into()));
    }
    if let Some(token) = progress {
        params.insert(
            "_meta".into(),
            serde_json::json!({ "progressToken": token }),
        );
    }
    params.insert("arguments".into(), Value::Object(arguments));
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": Value::Object(params),
    });
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Open a legacy session and return its id. Every tool call in this binary
/// runs on the legacy path (the 2026-07-28 stateless path is pinned by
/// `mcp_stateless.rs`), and rmcp refuses a `tools/call` before `initialize`.
async fn init_session(app: axum::Router) -> String {
    let res = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "initialize status");
    let sid = res
        .headers()
        .get(SESSION_HEADER)
        .or_else(|| res.headers().get("Mcp-Session-Id"))
        .expect("initialize must return a session id")
        .to_str()
        .unwrap()
        .to_string();
    let _ = body_json(res).await;
    sid
}

/// A `tools/call` on an already-open legacy session.
fn session_call(sid: &str, name: &str, query: &str, progress: Option<&str>) -> Request<Body> {
    let mut req = tool_call(name, query, progress);
    req.headers_mut()
        .insert(SESSION_HEADER, sid.parse().expect("session id header"));
    req
}

/// The `Content-Type` of a header set, normalized for comparison.
fn content_type_of(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Every request-log row currently in the event window, newest first.
async fn ring_rows(app: axum::Router) -> Vec<Value> {
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/request-logs?limit=200")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "admin list must authorize");
    body_json(res).await.as_array().cloned().unwrap_or_default()
}

/// Poll the admin request-log for a row matching `pred`. Emission is
/// synchronous, so the row is normally there on the first read; the poll is
/// belt-and-braces.
async fn poll_ring_row(app: axum::Router, pred: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..100 {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/request-logs?limit=200")
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "admin list must authorize");
        if let Some(row) = body_json(res)
            .await
            .as_array()
            .and_then(|rows| rows.iter().find(|r| pred(r)).cloned())
        {
            return row;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no request-log row matched");
}

// --- progress flush on the deadline exit -------------------------------------

/// A `tools/call` that sends a `progressToken` and parks in a vendor attempt
/// until the deadline must still end with the progress frames it queued, and
/// they must arrive BEFORE the terminal result.
///
/// This is the ordering contract `progress.rs` states (queued frames must
/// reach the transport before the terminal response, or rmcp's builder
/// cannot negotiate SSE): the Timeout arm flushes the sink before returning.
/// The assertion is on the INDEX of the progress frame relative to the
/// envelope, not on content-type — the legacy session path answers SSE for
/// every call whether or not a single frame was emitted, so the content type
/// proves nothing here. Two independent `contains` calls would not establish
/// order either, which is why the positions are compared.
#[tokio::test]
async fn mcp_deadline_exit_flushes_progress_before_the_terminal_result() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-flush").await.unwrap();
    let mut st = state_parked(db, spawn_blackhole());
    st.product_config = st
        .product_config
        .with_request_timeout(Duration::from_millis(300));
    let app = app(st);
    let sid = init_session(app.clone()).await;
    let res = app
        .clone()
        .oneshot(session_call(
            &sid,
            "search",
            "flush me",
            Some("tok-flush-1"),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "HTTP status");
    let text = String::from_utf8(body_bytes(res).await.to_vec()).unwrap();

    // The Attempt event fires as soon as the vendor leg is entered, so there
    // is always at least one queued frame when the deadline fires.
    let frame = text.find("notifications/progress").unwrap_or_else(|| {
        panic!("the queued progress frame must be delivered on the deadline path: {text}")
    });
    // The envelope rides in a JSON text block, so its quotes are escaped.
    let envelope = text
        .find(r#"\"kind\":\"Timeout\""#)
        .unwrap_or_else(|| panic!("the deadline must still answer a Timeout envelope: {text}"));
    assert!(
        frame < envelope,
        "the progress frame (at {frame}) must precede the terminal envelope (at {envelope}): {text}"
    );

    // The 504 row is emitted, unchanged by the flush.
    let row = poll_ring_row(app, |r| {
        r["path"] == "/mcp/search" && r["errorKind"] == "Timeout"
    })
    .await;
    assert_eq!(row["status"], 504, "timeout row: {row}");
}

// --- CORS preflight -----------------------------------------------------------

/// A browser preflight (`OPTIONS` + `Origin` + `Access-Control-Request-Method`)
/// must be answered from the allowlist alone — never 401, since a preflight
/// carries no credentials by design. With `MCP_ALLOWED_ORIGINS` listing the
/// requesting origin, the response carries the CORS allow headers.
#[tokio::test]
async fn mcp_preflight_answers_without_auth_when_origin_allowed() {
    let _guard = env_lock().await;
    std::env::set_var("MCP_ALLOWED_ORIGINS", "https://app.example.com");
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Origin", "https://app.example.com")
                .header("Access-Control-Request-Method", "POST")
                .header("Access-Control-Request-Headers", "content-type")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // 2xx, and crucially NOT 401 (a preflight never carries a token).
    assert!(
        res.status().is_success(),
        "preflight must not be a 401/403, got {}",
        res.status()
    );
    assert_eq!(
        res.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("https://app.example.com"),
        "allowlisted origin must be echoed in Access-Control-Allow-Origin"
    );
}

/// A preflight for a NON-allowlisted origin gets no
/// `Access-Control-Allow-Origin`, so the browser still refuses the real
/// request — the unauthenticated preflight path is not a way around the
/// allowlist.
#[tokio::test]
async fn mcp_preflight_for_foreign_origin_gets_no_allow_origin() {
    let _guard = env_lock().await;
    std::env::set_var("MCP_ALLOWED_ORIGINS", "https://app.example.com");
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Origin", "https://evil.example")
                .header("Access-Control-Request-Method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        res.headers().get("access-control-allow-origin").is_none(),
        "a non-allowlisted origin must get no Access-Control-Allow-Origin"
    );
}

/// With `MCP_ALLOWED_ORIGINS` UNSET, a preflight is still answered without a
/// token (never a 401) but carries NO CORS header, so a browser still cannot
/// call `/mcp`. The documented decision: an unset/empty allowlist means "no
/// browser support", never "allow any origin".
#[tokio::test]
async fn mcp_preflight_without_allowlist_is_unauthenticated_but_not_cors_enabled() {
    let _guard = env_lock().await;
    std::env::remove_var("MCP_ALLOWED_ORIGINS");
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/mcp")
                .header("host", "localhost")
                .header("Origin", "https://app.example.com")
                .header("Access-Control-Request-Method", "POST")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        res.status().is_success(),
        "preflight must not require a token even with no allowlist, got {}",
        res.status()
    );
    assert!(
        res.headers().get("access-control-allow-origin").is_none(),
        "an unset allowlist must not emit CORS headers"
    );
}

/// A REAL (non-OPTIONS) response to an allowlisted browser origin must expose
/// the session id for reading. `fetch` only surfaces CORS-safelisted response
/// headers, so without `Access-Control-Expose-Headers: …mcp-session-id…` a
/// browser client that completes preflight and `initialize` cannot read the
/// id it must echo on every session-scoped call — the CORS work would be
/// unusable for exactly the flow it exists to enable.
#[tokio::test]
async fn mcp_allowlisted_response_exposes_the_session_header() {
    let _guard = env_lock().await;
    std::env::set_var("MCP_ALLOWED_ORIGINS", "https://app.example.com");
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    // The `initialize` itself, carrying the browser Origin.
    let mut req = mcp_request(MCP_INIT_BODY);
    req.headers_mut()
        .insert("origin", "https://app.example.com".parse().unwrap());
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "initialize status");

    let exposed = res
        .headers()
        .get("access-control-expose-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        exposed.contains("mcp-session-id"),
        "Mcp-Session-Id must be exposed so the client can read it, got: {exposed:?}"
    );
    assert!(
        res.headers().get("access-control-allow-origin").is_some(),
        "an allowlisted origin's real response must carry ACAO too"
    );
    // And the header the exposure names is actually on the response.
    assert!(
        res.headers().get(SESSION_HEADER).is_some(),
        "initialize must return the session id the expose header promises"
    );
}

/// A configured origin that differs from what the browser sends only in CASE
/// must still match, and still get its `Access-Control-Allow-Origin`.
///
/// This is the reconciliation with rmcp: rmcp lowercases scheme/host when it
/// parses its allowlist, so it would SERVE `https://App.Example.com` while a
/// raw tower-http list comparison (byte-exact `Vec::contains`) withheld the
/// CORS header — the silent "works until you use a capital letter" breakage.
/// The configured entry is normalized at parse time so both halves agree.
///
/// Ports are deliberately NOT covered here: rmcp treats a CONFIGURED port as
/// an exact requirement (`a_port.is_none() || a_port == o_port`), while a
/// browser omits default ports from `Origin` — so `https://app.example.com:443`
/// in the config matches nothing, on either half. That is rmcp's rule and
/// this task cannot change it; operators must omit default ports. The CORS
/// half strips them defensively so it can never be the stricter of the two.
#[tokio::test]
async fn mcp_mixed_case_and_default_port_origin_still_matches() {
    let _guard = env_lock().await;
    std::env::set_var("MCP_ALLOWED_ORIGINS", "https://App.Example.COM");
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));

    // The browser sends the canonical, portless, lowercase serialization.
    let mut req = mcp_request(MCP_INIT_BODY);
    req.headers_mut()
        .insert("origin", "https://app.example.com".parse().unwrap());
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "initialize status");
    assert_eq!(
        res.headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok()),
        Some("https://app.example.com"),
        "a mixed-case config entry must match the browser's normalized Origin"
    );
}

/// A wildcard `*` in the allowlist is dropped with a warning, never mapped to
/// allow-any. This is a crash guard as much as a policy one: tower-http's
/// `AllowOrigin::list` panics on a wildcard entry, so a `*` that reached the
/// layer would take the process down at router build. The server must BOOT.
#[tokio::test]
async fn mcp_wildcard_allowed_origins_boots_and_allows_nothing() {
    let _guard = env_lock().await;
    std::env::set_var("MCP_ALLOWED_ORIGINS", "*");
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // Building the app is the assertion: `app(...)` constructs the MCP service
    // and its CORS layer, so a wildcard reaching `AllowOrigin::list` would
    // panic right here.
    let app = app(state_with(db));
    let mut req = mcp_request(MCP_INIT_BODY);
    req.headers_mut()
        .insert("origin", "https://anywhere.example".parse().unwrap());
    let res = app.oneshot(req).await.unwrap();
    assert!(
        res.headers().get("access-control-allow-origin").is_none(),
        "`*` must not become allow-any; a browser still cannot call /mcp: {:?}",
        res.headers()
    );
    // rmcp matches its own allowlist with a parsed (scheme, host, port)
    // comparison in which a `*` entry parses to nothing, so it rejects every
    // origin rather than admitting them. Either way the outcome is
    // "not allowed", never "allowed by wildcard" — the point of the test.
}

// --- session <-> token binding -------------------------------------------------

/// A session opened by token A must not be drivable or terminable by token
/// B: POST, GET SSE and DELETE with a foreign token all answer the same 404 an
/// UNKNOWN session id gets, so the header is not a probe for other tenants'
/// handles. The owner's session survives the refused foreign DELETE.
#[tokio::test]
async fn foreign_token_cannot_drive_or_delete_another_tokens_session() {
    const FOREIGN: &str = "tok-foreigntokenfortest000000000000";
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "owner").await.unwrap();
    db.insert_token(FOREIGN, "intruder").await.unwrap();
    let app = app(state_with(db));

    // Token A opens a legacy session.
    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    assert_eq!(init.status(), StatusCode::OK);
    let sid = init_session(app.clone()).await;
    let _ = body_json(init).await;

    let owner_call = |id: i64| {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("host", "localhost")
            .header("Authorization", format!("Bearer {TEST_TOKEN}"))
            .header(SESSION_HEADER, &sid)
            .header("content-type", "application/json")
            .header("accept", MCP_ACCEPT)
            .body(Body::from(format!(
                r#"{{"jsonrpc":"2.0","id":{id},"method":"tools/list"}}"#
            )))
            .unwrap()
    };

    // The OWNER can still drive it (proves the binding MATCHES its token,
    // rather than every session id being 404).
    let owner_post = app.clone().oneshot(owner_call(2)).await.unwrap();
    assert_eq!(
        owner_post.status(),
        StatusCode::OK,
        "owner drives own session"
    );

    // A foreign token: POST, GET SSE and DELETE are all 404.
    for (method, accept) in [
        ("POST", MCP_ACCEPT),
        ("GET", "text/event-stream"),
        ("DELETE", MCP_ACCEPT),
    ] {
        let mut b = Request::builder()
            .method(method)
            .uri("/mcp")
            .header("host", "localhost")
            .header("Authorization", format!("Bearer {FOREIGN}"))
            .header(SESSION_HEADER, &sid)
            .header("accept", accept);
        if method == "POST" {
            b = b.header("content-type", "application/json");
        }
        let res = app
            .clone()
            .oneshot(
                b.body(Body::from(
                    r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#,
                ))
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "foreign {method} must be 404, not {}",
            res.status()
        );
    }

    // The rejection must be INDISTINGUISHABLE from rmcp's own answer for an
    // id that never existed — same body, same header set. A different shape
    // (a problem+json, a content-type) would confirm "this id exists and
    // belongs to someone else", i.e. turn the header into an existence probe
    // for other tenants' handles.
    let foreign_404 = {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/mcp")
                    .header("host", "localhost")
                    .header("Authorization", format!("Bearer {FOREIGN}"))
                    .header(SESSION_HEADER, &sid)
                    .header("accept", MCP_ACCEPT)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        (
            res.status(),
            res.headers().clone(),
            String::from_utf8(body_bytes(res).await.to_vec()).unwrap(),
        )
    };
    let unknown_404 = {
        // Same app, same token, an id that was never issued: rmcp itself
        // answers this one.
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/mcp")
                    .header("host", "localhost")
                    .header("Authorization", format!("Bearer {FOREIGN}"))
                    .header(SESSION_HEADER, "deadbeef-dead-beef-dead-beefdeadbeef")
                    .header("accept", MCP_ACCEPT)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        (
            res.status(),
            res.headers().clone(),
            String::from_utf8(body_bytes(res).await.to_vec()).unwrap(),
        )
    };
    assert_eq!(
        foreign_404.0, unknown_404.0,
        "both must be 404 (foreign vs unknown)"
    );
    assert_eq!(
        foreign_404.2, unknown_404.2,
        "a foreign-token rejection must be byte-identical to rmcp's unknown-session 404"
    );
    assert_eq!(
        content_type_of(&foreign_404.1),
        content_type_of(&unknown_404.1),
        "the content-type must match rmcp's own (rmcp sets none), or the two \
         404s are distinguishable"
    );

    // The owner's session is still alive: the refused foreign DELETE
    // terminated nothing.
    let owner_after = app.oneshot(owner_call(4)).await.unwrap();
    assert_eq!(
        owner_after.status(),
        StatusCode::OK,
        "a refused foreign DELETE must not terminate the owner's session"
    );
}

// --- per-token in-flight cap --------------------------------------------------

/// With the token at its cap, the next call is REFUSED with the retryable
/// `KeyBusy` envelope (the same back-pressure shape as the key pool's) — and
/// the refusal happens before the per-request progress delivery task exists:
/// the call carried a `progressToken` yet the response stayed plain JSON with
/// no progress frame, which is only possible if no sink was constructed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_cap_refuses_over_cap_with_retryable_envelope() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    db.insert_api_key("tavily", "tvly-cap").await.unwrap();
    // Calls park in the vendor attempt for the whole test, so the admitted
    // ones hold their permits the entire time.
    // Keep a handle to the same pool so the test can observe live key leases
    // (`Db` is a cheap handle over one pool).
    let probe = db.clone();
    let app = app(state_parked(db, spawn_blackhole()));
    let sid = init_session(app.clone()).await;
    let cap = 8;

    // Fill the cap: 8 concurrent calls, each with its own progressToken.
    let mut held = Vec::new();
    for id in 0..cap {
        let router = app.clone();
        let sid = sid.clone();
        held.push(tokio::spawn(async move {
            let res = router
                .oneshot(session_call(
                    &sid,
                    "search",
                    &format!("cap {id}"),
                    Some(&format!("cap-{id}")),
                ))
                .await
                .unwrap();
            let _ = body_bytes(res).await;
        }));
    }
    // Wait until the cap is genuinely FULL before issuing the ninth call. The
    // signal is a live key lease: an admitted call reaches the vendor leg and
    // holds a lease row, and with `max_inflight: 1` on the single tavily key
    // exactly ONE such call can be inside a vendor attempt at a time — the
    // rest are parked in key-pool acquire, still holding their admission
    // permits. A fixed sleep would race instead: the ninth could be admitted
    // because an earlier call had already released its permit, and the test
    // would pass without ever exercising the cap.
    async fn live_leases(db: &serpotter_db::Db) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM api_key_leases")
            .fetch_one(db.pool())
            .await
            .expect("count live key leases")
    }
    let mut parked = 0;
    for _ in 0..200 {
        parked = live_leases(&probe).await;
        if parked > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        parked > 0,
        "a call must be parked inside a vendor attempt before the cap can be full"
    );
    // The premise also rules out the key layer doing the refusing: none of the
    // first `cap` calls was turned away.
    let rows = ring_rows(app.clone()).await;
    assert_eq!(
        rows.iter().filter(|r| r["errorKind"] == "KeyBusy").count(),
        0,
        "the first {cap} calls must all be admitted, never refused: {rows:?}"
    );
    let res = app
        .clone()
        .oneshot(session_call(
            &sid,
            "search",
            "one too many",
            Some("cap-over"),
        ))
        .await
        .unwrap();
    let text = String::from_utf8(body_bytes(res).await.to_vec()).unwrap();
    let v = common::parse_mcp_json_body(&text);
    let result = &v["result"];
    assert_eq!(result["isError"], true, "over-cap call must fail: {v}");
    let env = error_envelope(result);
    assert_eq!(env["kind"], "KeyBusy", "refusal kind: {v}");
    assert_eq!(env["retryable"], true, "refusal must be retryable: {v}");
    assert!(
        env["message"]
            .as_str()
            .unwrap_or("")
            .contains(&cap.to_string()),
        "message names the limit: {v}"
    );
    // No progress delivery task ran for the refused call: the refusal happens
    // BEFORE the sink is constructed, so no `notifications/progress` frame
    // carrying this call's token can appear. (The legacy session path answers
    // SSE for every call, so the content type itself proves nothing — the
    // absence of frames does.)
    assert!(
        !text.contains("notifications/progress"),
        "a refused call must not run a progress delivery task: {text}"
    );
    assert!(
        !text.contains("cap-over"),
        "the refused call's progress token must never reach the transport: {text}"
    );
    assert!(
        result["structuredContent"].is_null(),
        "an error result omits structuredContent: {v}"
    );

    // The refusal is visible to the operator as a 503/KeyBusy row. Its
    // `tokenName` is attributed straight from the TokenRow the auth middleware
    // already put in `Parts` — `admit` runs before `resolve_mcp_log_ctx`, so a
    // refused call performs no `get_token_by_value` query at all. (The "takes
    // no `db` handle" half of that is a compile-time fact about the signature,
    // not something this test can observe; the attributed row is the part it
    // can.)
    let row = poll_ring_row(app, |r| r["errorKind"] == "KeyBusy").await;
    assert_eq!(row["status"], 503, "cap refusal row: {row}");
    assert_eq!(row["tokenName"], "t", "refusal row is attributed: {row}");

    for h in held {
        h.abort();
    }
}

// --- health: storage fault -> error envelope + event ---------------------------

/// `health` against a broken database must return the standard error envelope
/// with `isError` (NOT a bespoke `isError:false` `{status:"not_ready"}` body)
/// and must emit a request row — the one tool an operator would use to detect
/// an outage cannot be the one that leaves no trace.
#[tokio::test]
async fn mcp_health_with_broken_db_is_error_envelope_and_logs_event() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // Auth resolves the token against the same Db, so the fault must leave
    // the `tokens` table readable or the request 401s before health runs.
    // Dropping `schema_version` is a storage fault the health probe alone
    // trips over.
    sqlx::query("DROP TABLE schema_version")
        .execute(db.pool())
        .await
        .expect("drop schema_version");
    let app = app(state_with(db));
    let sid = init_session(app.clone()).await;
    let res = app
        .clone()
        .oneshot(session_call(&sid, "health", "", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "HTTP status");
    let v = body_json(res).await;
    let result = &v["result"];
    assert_eq!(
        result["isError"], true,
        "a storage fault must be an error, not a healthy body: {v}"
    );
    let env = error_envelope(result);
    assert_eq!(env["kind"], "DatabaseError", "health fault kind: {v}");
    assert_eq!(
        env["retryable"], false,
        "DatabaseError is not retryable: {v}"
    );
    // The driver's own text (SQL, table names) never reaches the client.
    assert!(
        !env["message"].as_str().unwrap_or("").contains("SELECT"),
        "the storage error text stays server-side: {v}"
    );
    // And it emitted a request row, which the old bespoke handler never did.
    let row = poll_ring_row(app, |r| r["path"] == "/mcp/health").await;
    assert_eq!(row["errorKind"], "DatabaseError", "health fault row: {row}");
    assert_eq!(row["status"], 500, "health fault status: {row}");
}

/// A schema behind the build is a DEPLOYMENT fault, not a transient one:
/// `health` must say so with the `NotReady` envelope, `retryable: false`, and a
/// 503 row. `retryable: true` here would send an agent into a retry loop that
/// only a migration can end — which is exactly why `NotReady` sits in
/// `kind_retryable`'s non-retryable set alongside `DatabaseError`.
#[tokio::test]
async fn mcp_health_with_stale_schema_is_not_ready_and_not_retryable() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    // Age the schema below what this build expects, the state a
    // not-yet-migrated deployment is in.
    sqlx::query("UPDATE schema_version SET version = version - 1000 WHERE id = 1")
        .execute(db.pool())
        .await
        .expect("age the schema version");
    let app = app(state_with(db));
    let sid = init_session(app.clone()).await;
    let res = app
        .clone()
        .oneshot(session_call(&sid, "health", "", None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "HTTP status");
    let v = body_json(res).await;
    let result = &v["result"];
    assert_eq!(result["isError"], true, "a stale schema is a failure: {v}");
    let env = error_envelope(result);
    assert_eq!(env["kind"], "NotReady", "stale-schema kind: {v}");
    assert_eq!(
        env["retryable"], false,
        "only a migration fixes a stale schema; a retry loop would never end: {v}"
    );
    let row = poll_ring_row(app, |r| r["path"] == "/mcp/health").await;
    assert_eq!(row["status"], 503, "stale-schema row: {row}");
    assert_eq!(row["errorKind"], "NotReady", "stale-schema row kind: {row}");
}

/// A healthy database still answers `health` with the readiness body AND a
/// 200 request row — the event and error-envelope changes must not cost the
/// success path its bespoke body (the tool advertises no `outputSchema`).
#[tokio::test]
async fn mcp_health_ready_still_returns_body_and_logs_200_row() {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    let app = app(state_with(db));
    let sid = init_session(app.clone()).await;
    let res = app
        .clone()
        .oneshot(session_call(&sid, "health", "", None))
        .await
        .unwrap();
    let v = body_json(res).await;
    assert_eq!(v["result"]["isError"], false, "ready health: {v}");
    let text = v["result"]["content"][0]["text"]
        .as_str()
        .expect("health text block")
        .to_string();
    let body: Value = serde_json::from_str(&text).expect("health body JSON");
    assert_eq!(
        body["status"], "ready",
        "migrated fixture must be ready: {body}"
    );
    let row = poll_ring_row(app, |r| r["path"] == "/mcp/health").await;
    assert_eq!(row["status"], 200, "health ready row: {row}");
    assert!(
        row["errorKind"].is_null(),
        "a ready health is not an error: {row}"
    );
}
