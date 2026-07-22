#[tokio::test]
async fn migrate_sets_schema_version_6() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let v = db.schema_version().await.expect("version");
    assert_eq!(v, serpotter_db::EXPECTED_SCHEMA_VERSION);
    assert_eq!(v, 6);
    db.ping().await.expect("ping");
}

#[tokio::test]
async fn settings_social_enabled_roundtrip() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    assert_eq!(db.get_social_enabled().await.unwrap(), true);
    db.set_social_enabled(false).await.unwrap();
    assert_eq!(db.get_social_enabled().await.unwrap(), false);
    // New connection / same pool re-read
    assert_eq!(
        db.get_setting("social_enabled").await.unwrap().as_deref(),
        Some("false")
    );
}

#[tokio::test]
async fn insert_and_get_token_roundtrip() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let row = db
        .insert_token("tok-testtokenvalue000000000000000000", "ci")
        .await
        .expect("insert");
    assert!(row.id > 0);
    assert_eq!(row.name, "ci");
    let found = db
        .get_token_by_value("tok-testtokenvalue000000000000000000")
        .await
        .expect("get")
        .expect("some");
    assert_eq!(found.id, row.id);
    assert!(db.get_token_by_value("tok-missing").await.unwrap().is_none());
}

#[tokio::test]
async fn api_key_acquire_and_report() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-test-key")
        .await
        .expect("insert");
    let acquired = db.acquire_api_key("tavily").await.expect("acq").expect("some");
    assert_eq!(acquired.id, k.id);
    assert_eq!(acquired.key, "tvly-test-key");

    db.report_api_key_failure(k.id).await.unwrap();
    db.report_api_key_failure(k.id).await.unwrap();
    let mid = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(mid.consecutive_fails, 2);
    assert_eq!(mid.active, 1);

    db.report_api_key_failure(k.id).await.unwrap();
    let dead = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(dead.consecutive_fails, 3);
    assert_eq!(dead.active, 0);
    assert!(db.acquire_api_key("tavily").await.unwrap().is_none());
}

#[tokio::test]
async fn api_key_success_resets_fails() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-ok").await.unwrap();
    db.report_api_key_failure(k.id).await.unwrap();
    db.report_api_key_success(k.id).await.unwrap();
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.consecutive_fails, 0);
    assert_eq!(row.active, 1);
}

#[tokio::test]
async fn api_key_batch_distinct() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    db.insert_api_key("tavily", "tvly-1").await.unwrap();
    db.insert_api_key("tavily", "tvly-2").await.unwrap();
    let batch = db.acquire_api_keys_batch("tavily", 5).await.unwrap();
    assert_eq!(batch.len(), 2);
    assert_ne!(batch[0].id, batch[1].id);
}

#[tokio::test]
async fn acquire_prefers_positive_credits_over_zero() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    // Insert exhausted first (would win pure LRU if no priority)
    let zero = db.insert_api_key("tavily", "tvly-zero").await.unwrap();
    db.set_api_key_credits(zero.id, Some(0)).await.unwrap();
    let ok = db.insert_api_key("tavily", "tvly-ok").await.unwrap();
    // null credits = priority 1 (unknown); prefer over zero
    let acquired = db.acquire_api_key("tavily").await.unwrap().expect("some");
    assert_eq!(acquired.id, ok.id, "must prefer non-exhausted key");
    assert_eq!(acquired.key, "tvly-ok");
}

#[tokio::test]
async fn report_exhausted_zeros_credits_keeps_active() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-e").await.unwrap();
    db.set_api_key_credits(k.id, Some(50)).await.unwrap();
    db.report_api_key_exhausted(k.id).await.unwrap();
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1, "exhausted must not hard-disable");
    // Prove UPDATE zeroed credits (ApiKeyRow omits the column)
    let credits: Option<i64> = sqlx::query_scalar(
        "SELECT credits_remaining FROM api_keys WHERE id = ?",
    )
    .bind(k.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(credits, Some(0), "exhausted must zero credits_remaining");
    // still acquirable as priority-2 fallback when it is the only key
    let acquired = db.acquire_api_key("tavily").await.unwrap().expect("fallback");
    assert_eq!(acquired.id, k.id);
}

#[tokio::test]
async fn acquire_only_exhausted_still_returns_key() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-only-zero").await.unwrap();
    db.set_api_key_credits(k.id, Some(0)).await.unwrap();
    let acquired = db.acquire_api_key("tavily").await.unwrap().expect("some");
    assert_eq!(acquired.id, k.id);
}

#[tokio::test]
async fn acquire_sets_lease_and_blocks_second_until_expiry() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-lease").await.unwrap();
    let a = db.acquire_api_key("tavily").await.unwrap().expect("first");
    assert_eq!(a.id, k.id);
    // Still leased → no second key (only one)
    assert!(db.acquire_api_key("tavily").await.unwrap().is_none());
}

#[tokio::test]
async fn report_success_clears_lease_for_reacquire() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-clear").await.unwrap();
    let a = db.acquire_api_key("tavily").await.unwrap().unwrap();
    db.report_api_key_success(a.id).await.unwrap();
    let b = db.acquire_api_key("tavily").await.unwrap().expect("reacquire");
    assert_eq!(b.id, k.id);
}

#[tokio::test]
async fn expired_lease_is_stealable() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-steal").await.unwrap();
    db.acquire_api_key("tavily").await.unwrap().unwrap();
    // Force past lease
    sqlx::query("UPDATE api_keys SET lease_until = datetime('now', '-1 seconds') WHERE id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();
    let again = db.acquire_api_key("tavily").await.unwrap().expect("steal");
    assert_eq!(again.id, k.id);
}
