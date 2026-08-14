//! Wave 3A storage contracts (I1) exercised through the PUBLIC crate API:
//! B1 cache, B6 usage accumulation/spend, B16 jobs, B23 budget columns
//! (read-side). Request-log pagination is gone with the request_log table —
//! raw per-request events live in the in-memory ring + log stream (api crate).

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

/// B6: write-time upsert accumulation + spend aggregates, public API.
#[tokio::test]
async fn usage_accumulation_and_spend_public() {
    let db = db().await;
    let key = db.insert_api_key("tavily", "tvly-secret").await.unwrap();

    // Two per-request deltas, same day/key/token: success then failure.
    db.upsert_usage_daily("tavily", "tavily", key.id, "tok-a", 1, 1, 0, 33, 1.25)
        .await
        .unwrap();
    db.upsert_usage_daily("tavily", "tavily", key.id, "tok-a", 1, 0, 1, 0, 0.0)
        .await
        .unwrap();
    let rows = db.usage_summary(1).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].requests, 2);
    assert_eq!(rows[0].successes, 1);
    assert_eq!(rows[0].errors, 1);
    assert_eq!(rows[0].tokens, 33);
    assert!((rows[0].cost - 1.25).abs() < 1e-9);

    // Write-time upserts ADD: re-sending the same delta accumulates (the old
    // cron rollup replaced; the events writer is strictly additive).
    db.upsert_usage_daily("tavily", "tavily", key.id, "tok-a", 1, 1, 0, 33, 1.25)
        .await
        .unwrap();
    let rows = db.usage_summary(1).await.unwrap();
    assert_eq!(
        rows[0].requests, 3,
        "writer upserts accumulate, never replace"
    );

    // Spend per key/services (tok-a carries the accumulated cost).
    let by_key = db.spend_by_key().await.unwrap();
    assert_eq!(by_key.len(), 1);
    assert_eq!(by_key[0].token_name.as_deref(), Some("tok-a"));
    assert_eq!(by_key[0].service, "tavily");
    assert_eq!(by_key[0].requests, 3);
    assert!((by_key[0].cost - 2.5).abs() < 1e-9);
    let by_service = db.spend_by_service().await.unwrap();
    assert_eq!(by_service.len(), 1);
    assert!((by_service[0].cost - 2.5).abs() < 1e-9);
}
