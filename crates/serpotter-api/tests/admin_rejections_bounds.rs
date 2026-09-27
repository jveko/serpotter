//! T-adminapi: admin rejection shapes, bounded admin inputs, the shared
//! `days` clamp, the spend window, and the new request-log fields/filter.

mod common;

use common::*;
use serde_json::Value;
use serpotter_api::events::LogFields;
use serpotter_api::AppState;

fn content_type(res: &axum::response::Response) -> Option<String> {
    res.headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

fn admin_request(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn admin_post(uri: &str, body: &str) -> Request<Body> {
    admin_request("POST", uri, body)
}

/// Assert the response is an RFC 9457 problem+json of the given kind. The
/// body is read last, after the headers have been checked.
async fn assert_problem(res: axum::response::Response, status: StatusCode, tag: &str) {
    assert_eq!(res.status(), status, "expected {status} problem {tag}");
    assert_eq!(
        content_type(&res).as_deref(),
        Some("application/problem+json"),
        "rejection must be problem+json, not axum plain text"
    );
    let v = body_json(res).await;
    assert!(
        v["type"]
            .as_str()
            .unwrap_or("")
            .ends_with(&format!("/{tag}")),
        "expected type .../{tag}, got {v}"
    );
    assert_eq!(v["status"], status.as_u16(), "problem body: {v}");
}

// --- P2: no admin rejection path leaks axum plain text ---------------------

#[tokio::test]
async fn malformed_admin_body_is_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    for (method, uri) in [
        ("POST", "/api/keys"),
        ("POST", "/api/tokens"),
        ("POST", "/api/nodes"),
        // `/api/settings` is PUT-only; a POST would 405 before extraction.
        ("PUT", "/api/settings"),
        ("POST", "/api/keys/sync-credits"),
        ("POST", "/api/admin/login"),
        ("POST", "/api/admin/change-password"),
    ] {
        let res = app
            .clone()
            .oneshot(admin_request(method, uri, r#"{"service":"#))
            .await
            .unwrap();
        assert_problem(res, StatusCode::BAD_REQUEST, "InvalidJson").await;
    }
}

#[tokio::test]
async fn wrong_shaped_admin_body_is_422_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    // Valid JSON, missing required fields.
    let res = app
        .clone()
        .oneshot(admin_post("/api/keys", "{}"))
        .await
        .unwrap();
    assert_problem(res, StatusCode::UNPROCESSABLE_ENTITY, "InvalidJson").await;

    let res = app
        .clone()
        .oneshot(admin_post("/api/nodes", "{}"))
        .await
        .unwrap();
    assert_problem(res, StatusCode::UNPROCESSABLE_ENTITY, "InvalidJson").await;
}

#[tokio::test]
async fn missing_content_type_is_415_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tokens")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::from(r#"{"name":"web"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_problem(
        res,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "InvalidContentType",
    )
    .await;
}

#[tokio::test]
async fn unparseable_admin_query_is_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    // `limit` is an integer in ListLogsQuery: "abc" cannot deserialize.
    for uri in [
        "/api/request-logs?limit=abc",
        "/api/usage?days=abc",
        "/api/spend/keys?days=abc",
        "/api/spend/services?days=abc",
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_problem(res, StatusCode::BAD_REQUEST, "InvalidQuery").await;
    }
}

#[tokio::test]
async fn unparseable_admin_path_is_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    for (method, uri) in [
        ("PUT", "/api/keys/not-a-number"),
        ("DELETE", "/api/keys/not-a-number"),
        ("POST", "/api/keys/not-a-number/toggle"),
        ("PUT", "/api/nodes/not-a-number"),
        ("DELETE", "/api/tokens/not-a-number"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_problem(res, StatusCode::BAD_REQUEST, "InvalidPath").await;
    }
}

// --- P3: bounded admin string inputs ---------------------------------------

#[tokio::test]
async fn oversize_admin_strings_are_rejected() {
    let db = test_db().await;
    let app = app(state_with(db));
    let long = "x".repeat(257);

    let cases: Vec<(String, String)> = vec![
        (
            "/api/keys".to_owned(),
            format!(r#"{{"service":"tavily","key":"{long}"}}"#),
        ),
        ("/api/tokens".to_owned(), format!(r#"{{"name":"{long}"}}"#)),
        (
            "/api/nodes".to_owned(),
            format!(r#"{{"host":"{long}","port":8080}}"#),
        ),
    ];
    for (uri, body) in cases {
        let res = app.clone().oneshot(admin_post(&uri, &body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "uri {uri}");
        let v = body_json(res).await;
        assert_eq!(v["title"], "Validation Error", "uri {uri}: {v}");
        assert!(
            v["detail"].as_str().unwrap_or("").contains("256"),
            "detail must name the bound, got {v}"
        );
    }
}

#[tokio::test]
async fn node_host_syntax_is_validated_at_create() {
    let db = test_db().await;
    let app = app(state_with(db));
    // Authority smuggling only: a scheme, port, path, or credential in
    // `host` would repoint the proxy instead of failing at create.
    for host in [
        "http://proxy.example",
        "proxy.example:8080",
        "proxy.example/path",
        "user@proxy.example",
    ] {
        let res = app
            .clone()
            .oneshot(admin_post(
                "/api/nodes",
                &format!(r#"{{"host":"{host}","port":8080}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "host {host} must be rejected at create, not at dial time"
        );
        let v = body_json(res).await;
        assert!(
            v["detail"]
                .as_str()
                .unwrap_or("")
                .contains("not a valid host"),
            "detail must explain the host rule: {v}"
        );
    }
    // Ordinary names — including single-label ones, which resolve via
    // /etc/hosts / mDNS / private DNS — must still create.
    for host in [
        "proxy.example.com",
        "localhost",
        "privoxy",
        "127.0.0.1",
        "[::1]",
    ] {
        let res = app
            .clone()
            .oneshot(admin_post(
                "/api/nodes",
                &format!(r#"{{"host":"{host}","port":8080}}"#),
            ))
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::CREATED,
            "host {host} must be accepted"
        );
    }
}

#[tokio::test]
async fn oversize_node_host_is_rejected_on_update() {
    let db = test_db().await;
    let node = db
        .insert_node("proxy.example", 8080, None, None, "http")
        .await
        .unwrap();
    let app = app(state_with(db));
    let long = "x".repeat(257);
    let res = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/nodes/{}", node.id))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .body(Body::from(format!(r#"{{"host":"{long}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v = body_json(res).await;
    assert!(
        v["detail"].as_str().unwrap_or("").contains("256"),
        "detail must name the bound, got {v}"
    );
}

/// The credential-rotation path must apply the same 256-char bound as every
/// other admin string, and the 400 must name the field that was sent.
#[tokio::test]
async fn node_credentials_are_bounded_on_update() {
    let db = test_db().await;
    let node = db
        .insert_node("proxy.example", 8080, None, None, "http")
        .await
        .unwrap();
    let app = app(state_with(db));

    let update = |field: &'static str, value: String| {
        let app = app.clone();
        let uri = format!("/api/nodes/{}", node.id);
        async move {
            let res = app
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(&uri)
                        .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                        .header("content-type", "application/json")
                        .body(Body::from(format!(r#"{{"{field}":"{value}"}}"#)))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = res.status();
            let ct = content_type(&res).map(|s| s.to_string());
            (status, ct, body_json(res).await)
        }
    };

    for field in ["username", "password"] {
        let (status, _, v) = update(field, "p".repeat(300)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{field}: an oversize credential must be rejected"
        );
        let detail = v["detail"].as_str().unwrap_or("");
        assert!(
            detail.contains(field) && detail.contains("256"),
            "detail must name the field and the bound, got {detail:?}"
        );
    }

    // A normal value is stored.
    let (status, ct, _) = update("username", "rotated-user".to_owned()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a normal credential is accepted: {ct:?}"
    );

    // An explicit empty string is a legitimate "set to empty", not a bound
    // violation.
    let (status, _, _) = update("password", String::new()).await;
    assert_eq!(status, StatusCode::OK, "empty credential must be allowed");
}

/// tower-http's `RequestBodyLimit` short-circuits on a declared
/// over-limit `Content-Length` before any extractor runs, so the 413 must
/// still reach the client as problem+json on the admin surface exactly as it
/// does on the product surface.
#[tokio::test]
async fn oversize_admin_body_with_content_length_is_413_problem_json() {
    let db = test_db().await;
    let app = app(state_with(db));
    let pad = "k".repeat(serpotter_api::BODY_LIMIT_BYTES + 128 * 1024);
    let payload = format!(r#"{{"name":"{pad}"}}"#);

    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tokens")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .header("content-type", "application/json")
                .header("content-length", payload.len().to_string())
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        content_type(&res).as_deref(),
        Some("application/problem+json"),
        "the layer-level 413 must not be plain text on the admin surface"
    );
    let v = body_json(res).await;
    assert!(v["type"].as_str().unwrap_or("").ends_with("/BodyTooLarge"));
}

// --- P1: one shared days clamp ---------------------------------------------

/// Backdate every `usage_daily` row to `days` ago (relative: no UTC flake).
async fn backdate_usage(db: &serpotter_db::Db, days: i64) {
    sqlx::query("UPDATE usage_daily SET date = date('now', '-' || ? || ' days')")
        .bind(days)
        .execute(db.pool())
        .await
        .unwrap();
}

async fn seed_one_usage_row(db: &serpotter_db::Db) {
    let k = db.insert_api_key("tavily", "tvly-window").await.unwrap();
    db.upsert_usage_daily("tavily", "tavily", k.id, "tok-window", 1, 1, 0, 10, 1.0)
        .await
        .unwrap();
}

async fn usage_rows(app: &axum::Router, days: &str) -> Value {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/usage?days={days}"))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await
}

#[tokio::test]
async fn usage_window_reaches_past_ninety_days() {
    let db = test_db().await;
    seed_one_usage_row(&db).await;
    // Day 120: outside the old hard-coded 90-day ceiling.
    backdate_usage(&db, 120).await;
    let app = app(state_with(db));

    let rows = usage_rows(&app, "90").await;
    assert!(
        rows.as_array().expect("array").is_empty(),
        "a 90-day window must not reach 120 days back"
    );
    let wide = usage_rows(&app, "120").await;
    assert_eq!(
        wide.as_array().expect("array").len(),
        1,
        "a 120-day window must include the day-120 row"
    );
}

#[tokio::test]
async fn usage_days_above_the_shared_bound_clamp_to_it() {
    let db = test_db().await;
    seed_one_usage_row(&db).await;
    // Day 179: inside the shared 180-day bound, outside any 90-day default.
    backdate_usage(&db, 179).await;
    let app = app(state_with(db));

    let clamped = usage_rows(&app, "200").await;
    assert_eq!(
        clamped.as_array().expect("array").len(),
        1,
        "days=200 must clamp to the shared 180-day bound and still see day 179"
    );
    assert_eq!(
        clamped,
        usage_rows(&app, "180").await,
        "clamp equals the bound"
    );
}

#[tokio::test]
async fn spend_endpoints_only_return_rows_inside_the_window() {
    let db = test_db().await;
    let k = db.insert_api_key("tavily", "tvly-spend").await.unwrap();
    db.upsert_usage_daily("tavily", "tavily", k.id, "tok-old", 1, 1, 0, 0, 5.0)
        .await
        .unwrap();
    backdate_usage(&db, 40).await;
    db.upsert_usage_daily("firecrawl", "firecrawl", 0, "tok-new", 1, 1, 0, 0, 1.0)
        .await
        .unwrap();
    let app = app(state_with(db));

    let get = |app: &axum::Router, url: String| {
        let app = app.clone();
        async move {
            let res = app
                .oneshot(
                    Request::builder()
                        .uri(&url)
                        .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "url {url}");
            body_json(res).await
        }
    };

    let keys = get(&app, "/api/spend/keys?days=7".to_owned()).await;
    let rows = keys.as_array().expect("spend keys array");
    assert_eq!(
        rows.len(),
        1,
        "40-day-old spend is outside a 7-day window: {keys}"
    );
    assert_eq!(rows[0]["tokenName"], "tok-new");

    let services = get(&app, "/api/spend/services?days=7".to_owned()).await;
    let rows = services.as_array().expect("spend services array");
    assert_eq!(rows.len(), 1, "service window: {services}");
    assert_eq!(rows[0]["service"], "firecrawl");

    // The default 90-day window still shows the 40-day-old row.
    let wide = get(&app, "/api/spend/keys".to_owned()).await;
    let rows = wide.as_array().expect("spend keys array");
    assert_eq!(rows.len(), 2, "default 90d window sees both: {wide}");
    assert_eq!(
        rows[0]["tokenName"], "tok-old",
        "cost DESC keeps the spender first"
    );
}

#[tokio::test]
async fn spend_days_above_the_shared_bound_clamp_to_it() {
    let db = test_db().await;
    let k = db
        .insert_api_key("tavily", "tvly-spend-clamp")
        .await
        .unwrap();
    db.upsert_usage_daily("tavily", "tavily", k.id, "tok-old", 1, 1, 0, 0, 5.0)
        .await
        .unwrap();
    backdate_usage(&db, 179).await;
    let app = app(state_with(db));

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/spend/keys?days=200")
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(
        v.as_array().expect("array").len(),
        1,
        "days=200 must clamp to 180 and still see day 179: {v}"
    );
}

// --- P3: collected-but-unexposed request data ------------------------------

fn usage_fields(request_id: &str, error_kind: Option<&'static str>, cache_hit: bool) -> LogFields {
    LogFields {
        path: "/api/search",
        status: if error_kind.is_some() { 502 } else { 200 },
        duration_ms: Some(7),
        service: Some("tavily".into()),
        provider_used: Some("tavily".into()),
        error_kind,
        query_preview: Some("q".into()),
        request_id: Some(request_id.into()),
        token_name: Some("tok-usage-fields".into()),
        strategy: Some("hybrid".into()),
        providers_consulted: Some("tavily".into()),
        attempt_count: Some(1),
        key_id: None,
        node_id: None,
        input_tokens: Some(120),
        output_tokens: Some(80),
        total_tokens: Some(200),
        cost_est: Some(0.0042),
        cache_hit,
        attempt_log: Vec::new(),
    }
}

async fn logs(state: AppState, query: &str) -> Value {
    let res = app(state)
        .oneshot(
            Request::builder()
                .uri(format!("/api/request-logs?{query}"))
                .header("Authorization", format!("Bearer {TEST_ADMIN_SECRET}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    body_json(res).await
}

#[tokio::test]
async fn log_rows_expose_tokens_cost_and_cache_hit() {
    let db = test_db().await;
    let state = state_with(db);
    state.events.test_push(usage_fields("fields-a", None, true));
    state
        .events
        .test_push(usage_fields("fields-b", Some("UpstreamError"), false));
    let rows = logs(state, "limit=10").await;

    let row = rows
        .as_array()
        .expect("logs array")
        .iter()
        .find(|r| r["requestId"] == "fields-a")
        .expect("fields-a row")
        .clone();
    assert_eq!(row["cacheHit"], true);
    assert_eq!(row["inputTokens"], 120);
    assert_eq!(row["outputTokens"], 80);
    assert_eq!(row["totalTokens"], 200);
    assert!((row["costEst"].as_f64().unwrap() - 0.0042).abs() < 1e-9);

    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["requestId"] == "fields-b")
        .expect("fields-b row")
        .clone();
    assert_eq!(row["cacheHit"], false, "cacheHit is always present");
}

#[tokio::test]
async fn error_kind_filter_selects_only_matching_rows() {
    let db = test_db().await;
    let state = state_with(db);
    state.events.test_push(usage_fields("kind-ok", None, false));
    state
        .events
        .test_push(usage_fields("kind-err", Some("UpstreamError"), false));
    state
        .events
        .test_push(usage_fields("kind-err2", Some("UpstreamError"), false));

    let rows = logs(state, "errorKind=UpstreamError").await;
    let ids: Vec<_> = rows
        .as_array()
        .expect("logs array")
        .iter()
        .map(|r| r["requestId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids,
        vec!["kind-err2", "kind-err"],
        "only the matching errorKind, newest first: {rows}"
    );
}

#[tokio::test]
async fn error_kind_filter_never_matches_a_success_row() {
    let db = test_db().await;
    let state = state_with(db);
    state.events.test_push(usage_fields("only-ok", None, false));
    let rows = logs(state, "errorKind=Unauthorized").await;
    assert!(
        rows.as_array().expect("logs array").is_empty(),
        "a success row has no errorKind and must not match: {rows}"
    );
}

/// The wire round-trip for the `errorKind` filter: the value a caller PUSHED
/// is the value it can FILTER on, in both directions — `Timeout` returns the
/// row, and a different kind excludes it. Exact-match, not substring or
/// case-insensitive.
#[tokio::test]
async fn error_kind_filter_round_trips_the_pushed_value() {
    let db = test_db().await;
    let state = state_with(db);
    state
        .events
        .test_push(usage_fields("kind-timeout", Some("Timeout"), false));

    let rows = logs(state.clone(), "errorKind=Timeout").await;
    let ids: Vec<_> = rows
        .as_array()
        .expect("logs array")
        .iter()
        .map(|r| r["requestId"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        ids,
        vec!["kind-timeout"],
        "pushed kind must filter back: {rows}"
    );

    let rows = logs(state, "errorKind=Other").await;
    assert!(
        rows.as_array().expect("logs array").is_empty(),
        "a different errorKind must exclude the row: {rows}"
    );
}

// --- admin DatabaseError must not echo driver text ------------------------

/// Authenticate with `X-Admin-Password` — the one admin credential path that
/// touches NO database — and then close the pool. A session Bearer would
/// instead fail inside `require_admin`'s own `get_valid_admin_session` lookup
/// (mod.rs:75-81), so every request would 500 from auth and none of the ~44
/// handler `database_problem` sites would ever run. With the DB-free header,
/// auth succeeds with the pool closed and the failure can only come from the
/// handler's own query.
#[tokio::test]
async fn admin_db_error_returns_generic_detail_without_driver_text() {
    let db = test_db().await;
    let app = app(state_with(db.clone()));

    async fn call(app: axum::Router, uri: &str) -> (StatusCode, Value) {
        let res = app
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-admin-password", TEST_ADMIN_SECRET)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        (status, body_json(res).await)
    }

    // Sanity: with a live pool the same header authorizes a real read, so the
    // 500s below are the closed pool and not a rejected credential.
    let (status, _) = call(app.clone(), "/api/keys").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "X-Admin-Password must authorize first"
    );

    db.pool().close().await;

    // One route per handler family. Each runs its own query after auth, so
    // each exercises that handler's `database_problem` arm.
    for uri in [
        "/api/keys",
        "/api/tokens",
        "/api/nodes",
        "/api/settings",
        "/api/stats",
        "/api/usage",
        "/api/spend/keys",
        "/api/spend/services",
    ] {
        let (status, v) = call(app.clone(), uri).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "uri {uri} must surface the storage fault from its own query"
        );
        assert_eq!(v["title"], "Database Error", "uri {uri}: {v}");
        let detail = v["detail"].as_str().unwrap_or("");
        assert_eq!(detail, "internal storage error", "uri {uri} fixed detail");
        for leak in [
            "no such table",
            "database is locked",
            "sqlite",
            "pool",
            "sqlx",
        ] {
            assert!(
                !detail.to_lowercase().contains(leak),
                "uri {uri} leaked driver wording {leak:?}: {detail}"
            );
        }
    }

    // `require_admin`'s OWN arm: a session Bearer makes the session lookup run
    // first, and a closed pool fails it. Labelled separately because it is a
    // different call site than the handlers above.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/keys")
                .header("Authorization", "Bearer adm-session-lookup-probe")
                .header("x-admin-password", TEST_ADMIN_SECRET)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let v = body_json(res).await;
    assert_eq!(v["title"], "Database Error", "require_admin arm: {v}");
    assert_eq!(v["detail"], "internal storage error");
}
