//! Research-surface PARITY: `/api/research` (REST) and the MCP `research`
//! tool must accept and refuse the same requests with the same detail text.
//!
//! The rules under test are the three research-boundary rules core owns —
//! `time_range` through `normalize_time_range`, the deep-loop combination
//! rule (`validate_deep_research_knobs`), and the two closed sets. They used
//! to drift: research's `time_range` was forwarded raw on both surfaces, and
//! the deep-combo rule existed on NEITHER, so the deep loop silently dropped
//! the Tavily backend and the citation format it was given.
//!
//! Every request below is answered with NO provider keys configured, so an
//! accepted request deterministically ends in `NoHealthyKey` (a provider
//! answer) and a refused one in a 400 carrying the shared detail string. The
//! comparison is on the DETAIL, not the status code: the two surfaces have
//! different transports (MCP wraps the RFC 9457 body in a tool-result
//! envelope), and the contract is that they describe the same refusal.

mod common;

use axum::body::Body;
use axum::http::Request;
use common::*;
use serde_json::Value;

/// POST `/api/research` with `body`; returns `(status, problem body)`.
async fn rest_research(app: &axum::Router, body: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/research")
                .header("Authorization", format!("Bearer {TEST_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    (status, body_json(res).await)
}

/// MCP `tools/call` for the `research` tool with `arguments`. Returns
/// `(message, kind)` where `message` is the REFUSAL text — parsed out of the
/// tool-result error envelope when the call was refused, and the raw content
/// otherwise — so a test can compare it with REST's problem `detail`
/// DECODED on both sides (the envelope embeds the detail as a JSON string,
/// so a raw substring compare would trip over `\"` escaping).
async fn mcp_research(app: &axum::Router, sid: &str, arguments: Value) -> (String, String) {
    let res = app
        .clone()
        .oneshot(mcp_session_request(
            sid,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "research", "arguments": arguments},
            })
            .to_string(),
        ))
        .await
        .unwrap();
    let v = body_json(res).await;
    let text = v["result"]["content"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|c| c.get("text").or_else(|| c.get("Text")))
        .and_then(|t| t.as_str())
        .unwrap_or_else(|| panic!("no content text: {v}"))
        .to_string();
    match serde_json::from_str::<Value>(&text) {
        Ok(env) if env["kind"] == "ValidationError" => (
            env["message"]
                .as_str()
                .unwrap_or_else(|| panic!("envelope must carry `message`: {env}"))
                .to_string(),
            "ValidationError".to_string(),
        ),
        // Not a refusal: an accepted call (answer or a provider error) whose
        // kind is not ValidationError, so a "must be refused" assertion fails
        // with the actual outcome in the message.
        _ => (text, "not-a-validation-error".to_string()),
    }
}

async fn parity_app() -> axum::Router {
    let db = test_db().await;
    db.insert_token(TEST_TOKEN, "t").await.unwrap();
    app(state_with(db))
}

async fn init_sid(app: &axum::Router) -> String {
    let init = app
        .clone()
        .oneshot(mcp_request(MCP_INIT_BODY))
        .await
        .unwrap();
    let sid = init
        .headers()
        .get("mcp-session-id")
        .or_else(|| init.headers().get("Mcp-Session-Id"))
        .expect("Mcp-Session-Id")
        .to_str()
        .unwrap()
        .to_string();
    let _ = body_json(init).await;
    sid
}

/// An authenticated MCP POST bound to an established session.
fn mcp_session_request(sid: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("accept", MCP_ACCEPT)
        .header("Authorization", format!("Bearer {TEST_TOKEN}"))
        .header("mcp-session-id", sid)
        .body(body.into())
        .unwrap()
}

/// The REST problem `detail` for a refused request, and `None` when accepted.
fn rest_detail(status: StatusCode, v: &Value) -> String {
    assert_eq!(status, StatusCode::BAD_REQUEST, "not a refusal: {v}");
    v["detail"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| panic!("problem body must carry `detail`: {v}"))
}

// --- 1. time_range ---------------------------------------------------------

/// `timeRange:"W"` — Tavily's documented short form — is accepted on BOTH
/// surfaces: it is the spelling a client copies out of the vendor docs, and
/// research used to forward it raw on both while search rejected a typo.
#[tokio::test]
async fn short_time_range_is_accepted_on_both_surfaces() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    let (status, v) = rest_research(&app, r#"{"query":"x","timeRange":"W"}"#).await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "short form must pass: {v}");
    let (text, kind) = mcp_research(
        &app,
        &sid,
        serde_json::json!({"query": "x", "timeRange": "W"}),
    )
    .await;
    assert_ne!(kind, "ValidationError", "short form must pass: {text}");
}

/// A typo is a 400 on BOTH surfaces carrying the SAME text (core's
/// `normalize_time_range` message, naming the value and the members).
#[tokio::test]
async fn bogus_time_range_is_refused_identically() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    let (status, v) = rest_research(&app, r#"{"query":"x","timeRange":"nonsense"}"#).await;
    let rest = rest_detail(status, &v);
    let (text, kind) = mcp_research(
        &app,
        &sid,
        serde_json::json!({"query": "x", "timeRange": "nonsense"}),
    )
    .await;
    assert_eq!(kind, "ValidationError", "MCP must refuse too: {text}");
    assert_eq!(text, rest, "the two surfaces must answer identically");
    assert!(rest.contains("\"nonsense\""), "must name the value: {rest}");
}

// --- 2. deep + a knob the deep loop drops ---------------------------------

/// `deep` runs the serpotter loop, which never dials the Tavily backend and
/// never sends a citation format: naming either produced a confident answer
/// from a DIFFERENT product. Both surfaces now refuse, with one message.
#[tokio::test]
async fn deep_with_dropped_knob_is_refused_identically() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    for (field, value) in [
        ("researchBackend", serde_json::json!("tavily")),
        ("citationFormat", serde_json::json!("mla")),
        ("socialMaxResults", serde_json::json!(5)),
    ] {
        let body = serde_json::json!({"query": "x", "deep": true, field: value}).to_string();
        let (status, v) = rest_research(&app, &body).await;
        let rest = rest_detail(status, &v);
        let (text, kind) = mcp_research(
            &app,
            &sid,
            serde_json::from_str(&body).expect("arguments json"),
        )
        .await;
        assert_eq!(kind, "ValidationError", "{field}: MCP must refuse: {text}");
        assert_eq!(
            text, rest,
            "{field}: the two surfaces must answer identically"
        );
        assert!(rest.contains(field), "{field}: must name the knob: {rest}");
    }
}

/// The knobs the deep loop HONORS stay legal on both surfaces: an explicit
/// `socialMaxResults: 0` is the documented "social disabled" no-op, and
/// `scrapeTopN: 0` is what the loop's clamp treats as "scrape nothing".
#[tokio::test]
async fn deep_with_zero_dials_is_accepted_on_both_surfaces() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    let (status, v) = rest_research(
        &app,
        r#"{"query":"x","deep":true,"socialMaxResults":0,"scrapeTopN":0}"#,
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "zero dials must pass: {v}");
    let (text, kind) = mcp_research(
        &app,
        &sid,
        serde_json::json!({
            "query": "x", "deep": true, "socialMaxResults": 0, "scrapeTopN": 0
        }),
    )
    .await;
    assert_ne!(kind, "ValidationError", "zero dials must pass: {text}");
}

/// A BLANK dropped knob is "unset" on both surfaces (`normalize_choice` and
/// `research_inner` both fold `""` to `None`), so `deep` plus a defaulted
/// field must not be told it conflicts with itself.
#[tokio::test]
async fn deep_with_blank_dropped_knob_is_accepted() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    let (status, v) = rest_research(
        &app,
        r#"{"query":"x","deep":true,"researchBackend":"","citationFormat":"  "}"#,
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "blanks are unset: {v}");
    let (text, kind) = mcp_research(
        &app,
        &sid,
        serde_json::json!({"query": "x", "deep": true, "researchBackend": ""}),
    )
    .await;
    assert_ne!(kind, "ValidationError", "blanks are unset: {text}");
}

/// The same knobs WITHOUT `deep` are the standard path: both surfaces keep
/// them (the Tavily backend leg is the one that spends `citationFormat`).
#[tokio::test]
async fn standard_research_keeps_every_knob() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    let (status, v) = rest_research(
        &app,
        r#"{"query":"x","researchBackend":"tavily","citationFormat":"mla","socialMaxResults":5}"#,
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "must pass: {v}");
    let (text, kind) = mcp_research(
        &app,
        &sid,
        serde_json::json!({
            "query": "x",
            "researchBackend": "tavily",
            "citationFormat": "mla",
            "socialMaxResults": 5,
        }),
    )
    .await;
    assert_ne!(kind, "ValidationError", "must pass: {text}");
}

// --- 3. the closed sets (unchanged rules, now pinned on both) -------------

/// An unknown backend / citation format is still a 400 naming the field and
/// the members — the rule the REST boundary already owned.
#[tokio::test]
async fn bogus_backend_and_citation_are_refused_identically() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    for body in [
        r#"{"query":"x","researchBackend":"tavilyy"}"#,
        r#"{"query":"x","citationFormat":"footnote"}"#,
    ] {
        let (status, v) = rest_research(&app, body).await;
        let rest = rest_detail(status, &v);
        let (text, kind) = mcp_research(
            &app,
            &sid,
            serde_json::from_str(body).expect("arguments json"),
        )
        .await;
        assert_eq!(kind, "ValidationError", "{body}: MCP must refuse: {text}");
        assert_eq!(
            text, rest,
            "{body}: the two surfaces must answer identically"
        );
    }
}

/// Spelling variants of a real member are accepted by both (they name exactly
/// one advertised value; the canonical rewrite is the product entry's job).
#[tokio::test]
async fn member_spellings_are_accepted_on_both_surfaces() {
    let app = parity_app().await;
    let sid = init_sid(&app).await;
    let (status, v) = rest_research(
        &app,
        r#"{"query":"x","researchBackend":" Tavily ","citationFormat":"MLA","timeRange":"WEEK "}"#,
    )
    .await;
    assert_ne!(status, StatusCode::BAD_REQUEST, "spellings must pass: {v}");
    let (text, kind) = mcp_research(
        &app,
        &sid,
        serde_json::json!({
            "query": "x",
            "researchBackend": " Tavily ",
            "citationFormat": "MLA",
            "timeRange": "WEEK ",
        }),
    )
    .await;
    assert_ne!(kind, "ValidationError", "spellings must pass: {text}");
}
