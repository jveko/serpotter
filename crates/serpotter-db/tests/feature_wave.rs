//! Wave 3A storage contracts (I1) exercised through the PUBLIC crate API:
//! B1 cache, B6 usage rollup/spend, B16 jobs, B13 request-log pagination +
//! token_name filter, B23 budget columns (read-side).

use serpotter_db::RequestLogFilter;

async fn db() -> serpotter_db::Db {
    serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate")
}

/// B1: get/put roundtrip + expiry purge through the public surface.
#[tokio::test]
async fn cache_ttl_lifecycle_public() {
    let db = db().await;
    db.cache_put("tavily", "abc123", r#"{"ok":1}"#, 300)
        .await
        .unwrap();
    assert_eq!(
        db.cache_get("tavily", "abc123").await.unwrap().as_deref(),
        Some(r#"{"ok":1}"#)
    );
    // Expire it directly, then purge removes exactly it.
    sqlx::query("UPDATE query_cache SET expires_at = datetime('now', '-1 second') WHERE key_hash = 'abc123'")
        .execute(db.pool())
        .await
        .unwrap();
    assert_eq!(db.cache_get("tavily", "abc123").await.unwrap(), None);
    assert_eq!(db.purge_expired_cache().await.unwrap(), 1);
    assert_eq!(
        db.purge_expired_cache().await.unwrap(),
        0,
        "nothing left to purge"
    );
}

/// B6: rollup correctness + idempotency + spend aggregates, public API.
#[tokio::test]
async fn usage_rollup_and_spend_public() {
    let db = db().await;
    let key = db.insert_api_key("tavily", "tvly-secret").await.unwrap();

    db.insert_request_log_full(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(10),
        None,
        None,
        None,
        Some("tok-a"),
        None,
        None,
        Some(1),
        Some(key.id),
        None,
        Some(11),
        Some(22),
        Some(33),
        Some(1.25),
        Some(3.5),
        Some("oneshot"),
    )
    .await
    .unwrap();
    db.insert_request_log_full(
        "/api/search",
        "POST",
        502,
        Some("tavily"),
        Some("tavily"),
        Some(8),
        Some("provider"),
        None,
        None,
        Some("tok-a"),
        None,
        None,
        Some(2),
        Some(key.id),
        None,
        None,
        None,
        None,
        Some(0.0),
        None,
        Some("oneshot"),
    )
    .await
    .unwrap();

    let written = db.rollup_usage_from_request_log(24).await.unwrap();
    assert!(written >= 1);
    let rows = db.usage_summary(1).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].requests, 2);
    assert_eq!(rows[0].successes, 1);
    assert_eq!(rows[0].errors, 1);
    assert_eq!(rows[0].tokens, 33);
    assert!((rows[0].cost - 1.25).abs() < 1e-9);

    // Idempotent re-roll.
    db.rollup_usage_from_request_log(24).await.unwrap();
    let rows = db.usage_summary(1).await.unwrap();
    assert_eq!(rows[0].requests, 2, "re-roll must replace, not double");

    // Spend per key/services (tok-a carries the 1.25 cost).
    let by_key = db.spend_by_key().await.unwrap();
    assert_eq!(by_key.len(), 1);
    assert_eq!(by_key[0].token_name.as_deref(), Some("tok-a"));
    assert_eq!(by_key[0].service, "tavily");
    assert_eq!(by_key[0].requests, 2);
    assert!((by_key[0].cost - 1.25).abs() < 1e-9);
    let by_service = db.spend_by_service().await.unwrap();
    assert_eq!(by_service.len(), 1);
    assert!((by_service[0].cost - 1.25).abs() < 1e-9);
}

/// B16: job lifecycle through the public API.
#[tokio::test]
async fn jobs_lifecycle_public() {
    let db = db().await;
    let job = db
        .create_job("j-1", "tavily_research", "tavily", r#"{"q":"hi"}"#, 3600)
        .await
        .unwrap();
    assert_eq!(job.status, "running");

    assert!(db
        .update_job_result("j-1", "done", Some(r#"{"answer":"ok"}"#), None)
        .await
        .unwrap());
    let done = db.get_job("j-1").await.unwrap().unwrap();
    assert_eq!(done.status, "done");
    assert_eq!(done.result_json.as_deref(), Some(r#"{"answer":"ok"}"#));

    let list = db.list_jobs(10).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "j-1");

    // Expiry purge: only expired rows go.
    db.create_job("j-2", "other", "firecrawl", "{}", 3600)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE provider_jobs SET expires_at = datetime('now', '-1 second') WHERE id = 'j-1'",
    )
    .execute(db.pool())
    .await
    .unwrap();
    assert_eq!(db.purge_expired_jobs().await.unwrap(), 1);
    assert!(db.get_job("j-1").await.unwrap().is_none());
    assert!(db.get_job("j-2").await.unwrap().is_some());
}

/// B13: pagination + token_name filter, public API.
#[tokio::test]
async fn request_log_pagination_and_token_filter_public() {
    let db = db().await;
    for i in 0..5 {
        let token = if i % 2 == 0 { "tok-even" } else { "tok-odd" };
        db.insert_request_log(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            None,
            None,
            None,
            None,
            None,
            Some(token),
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        if i < 4 {
            sqlx::query("UPDATE request_log SET created_at = datetime('now', '-' || ? || ' seconds') WHERE id = ?")
                .bind(5 - i)
                .bind(i + 1)
                .execute(db.pool())
                .await
                .unwrap();
        }
    }
    let filter = RequestLogFilter {
        limit: 2,
        offset: 2,
        ..Default::default()
    };
    let page = db.list_request_logs(filter).await.unwrap();
    assert_eq!(page.len(), 2);
    assert_eq!(page[0].id, 3, "offset skips the two newest");

    let evens = db
        .list_request_logs(RequestLogFilter {
            limit: 50,
            token_name: Some("tok-even".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(evens.len(), 3);
    assert!(evens
        .iter()
        .all(|r| r.token_name.as_deref() == Some("tok-even")));

    let odd_page = db
        .list_request_logs(RequestLogFilter {
            token_name: Some("tok-odd".into()),
            limit: 1,
            offset: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(odd_page.len(), 1, "filter + pagination compose");
}

/// B23: budget columns land on api_keys and are readable on ApiKeyRow
/// (read-side only; gating is the next wave).
#[tokio::test]
async fn api_key_budget_columns_readable() {
    let db = db().await;
    let key = db.insert_api_key("tavily", "tvly-budget").await.unwrap();
    assert_eq!(key.budget_daily, None);
    assert_eq!(key.budget_monthly, None);

    sqlx::query("UPDATE api_keys SET budget_daily = 10.5, budget_monthly = 120.25 WHERE id = ?")
        .bind(key.id)
        .execute(db.pool())
        .await
        .unwrap();

    let fetched = db.get_api_key(key.id).await.unwrap().unwrap();
    assert_eq!(fetched.budget_daily, Some(10.5));
    assert_eq!(fetched.budget_monthly, Some(120.25));

    // Acquire path also surfaces them (null-safe select).
    let acquired = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acquired.id, key.id);
    assert_eq!(acquired.budget_daily, Some(10.5));
    db.release_api_key_inflight(key.id).await.unwrap();
}

/// B23 GATE: a key with budget_daily below the service-window spend is not
/// acquirable — acquire returns None (the keypool then fails the request).
#[tokio::test]
async fn api_key_budget_exhausted_acquire_returns_none() {
    let db = db().await;
    let key = db
        .insert_api_key("tavily", "tvly-budget-daily")
        .await
        .unwrap();
    db.set_api_key_budgets(key.id, Some(Some(1.0)), None)
        .await
        .unwrap();
    // Service window spend for TODAY: 2.0 >= budget 1.0 → gate trips.
    // (usage_daily.date is 'YYYY-MM-DD', matched against date('now') in the gate.)
    db.upsert_usage_daily("tavily", "tavily", &today_str(), 2, 2, 0, 0, 2.0)
        .await
        .unwrap();
    let acquired = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap();
    assert!(
        acquired.is_none(),
        "budget-exhausted key must not be acquired (spend >= budget_daily)"
    );
}

/// B23 GATE: with one over-budget key and one unbudgeted key in the same
/// service, acquire skips the budgeted key and returns the unbudgeted one.
#[tokio::test]
async fn api_key_budget_gate_skips_to_next_candidate() {
    let db = db().await;
    let bounded = db.insert_api_key("tavily", "tvly-budgeted").await.unwrap();
    db.set_api_key_budgets(bounded.id, Some(Some(1.0)), None)
        .await
        .unwrap();
    let _free = db.insert_api_key("tavily", "tvly-free").await.unwrap();
    db.upsert_usage_daily("tavily", "tavily", &today_str(), 2, 2, 0, 0, 2.0)
        .await
        .unwrap();
    let acquired = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .expect("the unbudgeted key must be picked");
    assert_eq!(acquired.id, _free.id, "budgeted key skipped");
    db.release_api_key_inflight(acquired.id).await.unwrap();
}

/// B23 GATE: budget_monthly trips when the month-to-date spend meets it.
#[tokio::test]
async fn api_key_budget_monthly_gate_trips() {
    let db = db().await;
    let key = db
        .insert_api_key("tavily", "tvly-budget-monthly")
        .await
        .unwrap();
    db.set_api_key_budgets(key.id, None, Some(Some(5.0)))
        .await
        .unwrap();
    let month_start = today_str()[..8].to_string() + "01";
    db.upsert_usage_daily("tavily", "tavily", &month_start, 3, 3, 0, 0, 6.0)
        .await
        .unwrap();
    let acquired = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap();
    assert!(
        acquired.is_none(),
        "monthly budget 5.0 vs month spend 6.0 must gate the key"
    );
}

/// B23 admin roundtrip: set, read, clear through the public API.
#[tokio::test]
async fn api_key_budget_admin_roundtrip() {
    let db = db().await;
    let key = db.insert_api_key("exa", "exa-budget-rt").await.unwrap();
    db.set_api_key_budgets(key.id, Some(Some(2.5)), Some(Some(30.0)))
        .await
        .unwrap();
    let row = db.get_api_key(key.id).await.unwrap().unwrap();
    assert_eq!(row.budget_daily, Some(2.5));
    assert_eq!(row.budget_monthly, Some(30.0));

    // Clear semantics: Some(None) removes the cap, None keeps it.
    db.set_api_key_budgets(key.id, Some(None), None)
        .await
        .unwrap();
    let row = db.get_api_key(key.id).await.unwrap().unwrap();
    assert_eq!(row.budget_daily, None, "cleared back to unlimited");
    assert_eq!(row.budget_monthly, Some(30.0), "untouched by partial patch");

    // Admin row surface carries the caps.
    let admin = db.get_api_key_admin(key.id).await.unwrap().unwrap();
    assert_eq!(admin.budget_daily, None);
    assert_eq!(admin.budget_monthly, Some(30.0));
}

fn today_str() -> String {
    // UTC date('now') — matches the gate's day boundary.
    use std::process::Command;
    let out = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .expect("date");
    String::from_utf8(out.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}
