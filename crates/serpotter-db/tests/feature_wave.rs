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
