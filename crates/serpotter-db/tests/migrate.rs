#[tokio::test]
async fn migrate_sets_schema_version_9() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let v = db.schema_version().await.expect("version");
    assert_eq!(v, serpotter_db::EXPECTED_SCHEMA_VERSION);
    assert_eq!(v, 9);
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

#[tokio::test]
async fn update_api_key_usage_writes_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-u").await.unwrap();
    db.report_api_key_failure(k.id).await.unwrap();
    db.update_api_key_usage(k.id, 12, 100).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 12);
    let lim: i64 = sqlx::query_scalar("SELECT credits_limit FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(lim, 100);
    let synced: Option<String> =
        sqlx::query_scalar("SELECT usage_synced_at FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert!(synced.is_some());
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.consecutive_fails, 0);
}

#[tokio::test]
async fn list_active_keys_for_service_filters_and_orders() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let a = db.insert_api_key("tavily", "tvly-a").await.unwrap();
    let b = db.insert_api_key("tavily", "tvly-b").await.unwrap();
    db.insert_api_key("firecrawl", "fc-x").await.unwrap();
    db.set_api_key_active(b.id, false).await.unwrap();
    db.update_api_key_usage(a.id, 1, 10).await.unwrap();
    let never = db.insert_api_key("tavily", "tvly-never").await.unwrap();
    let listed = db.list_active_keys_for_service("tavily").await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, never.id, "never-synced first");
    assert_eq!(listed[1].id, a.id);
}

#[tokio::test]
async fn request_log_insert_and_purge() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    for i in 0..5 {
        db.insert_request_log(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            Some("tavily"),
            Some(10 + i),
            None,
            Some("hello"),
        )
        .await
        .expect("insert");
    }
    assert_eq!(db.count_request_logs().await.unwrap(), 5);
    // Cap to 2 newest
    let purged = db.purge_request_log(30, 2).await.expect("purge");
    assert!(purged >= 3);
    assert_eq!(db.count_request_logs().await.unwrap(), 2);
}

#[tokio::test]
async fn reenable_stale_keys_after_hours() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-stale")
        .await
        .expect("insert");
    db.set_api_key_active(k.id, false).await.unwrap();
    // Force last_used_at far in the past
    db.set_api_key_last_used_at(k.id, Some("2000-01-01 00:00:00"))
        .await
        .unwrap();
    let n = db.reenable_stale_keys(24).await.expect("reenable");
    assert_eq!(n, 1);
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1);
    assert_eq!(row.consecutive_fails, 0);
}

#[tokio::test]
async fn reenable_skips_recent_inactive() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-recent")
        .await
        .expect("insert");
    db.set_api_key_active(k.id, false).await.unwrap();
    // Recent activity: far future last_used so not older than now-24h
    db.set_api_key_last_used_at(k.id, Some("2099-01-01 00:00:00"))
        .await
        .unwrap();
    let n = db.reenable_stale_keys(24).await.expect("reenable");
    assert_eq!(n, 0);
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 0);
}

#[tokio::test]
async fn stats_by_service_aggregates() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let a = db.insert_api_key("tavily", "tvly-1").await.unwrap();
    let b = db.insert_api_key("tavily", "tvly-2").await.unwrap();
    db.insert_api_key("firecrawl", "fc-1").await.unwrap();
    db.set_api_key_active(b.id, false).await.unwrap();
    db.update_api_key_usage(a.id, 5, 100).await.unwrap();
    let stats = db.stats_by_service().await.unwrap();
    assert_eq!(stats.len(), 2);
    let tavily = stats.iter().find(|s| s.service == "tavily").unwrap();
    assert_eq!(tavily.keys, 2);
    assert_eq!(tavily.active, 1);
    assert_eq!(tavily.credits_remaining_sum, Some(5));
    assert_eq!(tavily.credits_limit_sum, Some(100));
}

#[tokio::test]
async fn admin_user_and_session_roundtrip() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    assert_eq!(db.count_admin_users().await.unwrap(), 0);
    let user = db
        .insert_admin_user("admin", "$argon2id$placeholder")
        .await
        .unwrap();
    assert_eq!(user.username, "admin");
    assert_eq!(db.count_admin_users().await.unwrap(), 1);
    let got = db.get_admin_user_by_username("admin").await.unwrap().unwrap();
    assert_eq!(got.id, user.id);
    assert_eq!(got.password_hash, "$argon2id$placeholder");

    let sess = db
        .insert_admin_session("sess-test-token", user.id, "2099-01-01 00:00:00")
        .await
        .unwrap();
    assert_eq!(sess.user_id, user.id);
    let valid = db
        .get_valid_admin_session("sess-test-token")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(valid.token, "sess-test-token");

    // expired session is not valid
    db.insert_admin_session("sess-expired", user.id, "2000-01-01 00:00:00")
        .await
        .unwrap();
    assert!(db
        .get_valid_admin_session("sess-expired")
        .await
        .unwrap()
        .is_none());

    assert!(db.delete_admin_session("sess-test-token").await.unwrap());
    assert!(db
        .get_valid_admin_session("sess-test-token")
        .await
        .unwrap()
        .is_none());
}

async fn key_inflight(db: &serpotter_db::Db, id: i64) -> i64 {
    sqlx::query_scalar("SELECT inflight FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

async fn key_lease(db: &serpotter_db::Db, id: i64) -> Option<String> {
    sqlx::query_scalar("SELECT lease_until FROM api_keys WHERE id = ?")
        .bind(id)
        .fetch_one(db.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn shared_acquire_allows_max_inflight_then_blocks() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-shared").await.unwrap();
    assert_eq!(db.count_active_keys("tavily").await.unwrap(), 1);

    for i in 1..=3 {
        let got = db
            .acquire_api_key_shared("tavily", 3, serpotter_db::KEY_HOLD_TTL_SECS)
            .await
            .unwrap()
            .expect("hold");
        assert_eq!(got.id, k.id);
        assert_eq!(key_inflight(&db, k.id).await, i);
        assert!(key_lease(&db, k.id).await.is_some());
    }
    assert!(db
        .acquire_api_key_shared("tavily", 3, serpotter_db::KEY_HOLD_TTL_SECS)
        .await
        .unwrap()
        .is_none());
    assert_eq!(key_inflight(&db, k.id).await, 3);
}

#[tokio::test]
async fn report_decrements_inflight_clears_lease_only_at_zero() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-dec").await.unwrap();
    for _ in 0..3 {
        db.acquire_api_key_shared("tavily", 3, 90)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(key_inflight(&db, k.id).await, 3);
    assert!(key_lease(&db, k.id).await.is_some());

    db.report_api_key_success(k.id).await.unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 2);
    assert!(
        key_lease(&db, k.id).await.is_some(),
        "lease kept while holds remain"
    );

    db.release_api_key_inflight(k.id).await.unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 1);
    assert!(key_lease(&db, k.id).await.is_some());

    db.report_api_key_exhausted(k.id).await.unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 0);
    assert!(
        key_lease(&db, k.id).await.is_none(),
        "lease cleared only at last hold"
    );
}

#[tokio::test]
async fn reclaim_expired_key_holds_zeros_inflight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-reclaim").await.unwrap();
    db.acquire_api_key_shared("tavily", 3, 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 1);

    sqlx::query("UPDATE api_keys SET lease_until = datetime('now', '-1 seconds') WHERE id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();
    let n = db.reclaim_expired_key_holds().await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(key_inflight(&db, k.id).await, 0);
    assert!(key_lease(&db, k.id).await.is_none());
}

#[tokio::test]
async fn zero_all_key_inflight_clears_holds() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-zero").await.unwrap();
    db.acquire_api_key_shared("tavily", 5, 90)
        .await
        .unwrap()
        .unwrap();
    db.zero_all_key_inflight().await.unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 0);
    assert!(key_lease(&db, k.id).await.is_none());
}

#[tokio::test]
async fn acquire_outbound_node_prefers_least_inflight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let a = db
        .insert_node("a.example", 8080, None, None)
        .await
        .unwrap();
    let b = db
        .insert_node("b.example", 8080, None, None)
        .await
        .unwrap();
    let first = db.acquire_outbound_node().await.unwrap().unwrap();
    assert_eq!(first.id, a.id);
    assert_eq!(first.inflight, 1);
    let second = db.acquire_outbound_node().await.unwrap().unwrap();
    assert_eq!(second.id, b.id, "prefer other node when inflight differs");
    assert_eq!(second.inflight, 1);

    let nodes = db.list_nodes().await.unwrap();
    assert_eq!(nodes.iter().find(|n| n.id == a.id).unwrap().inflight, 1);
    assert_eq!(nodes.iter().find(|n| n.id == b.id).unwrap().inflight, 1);
}

#[tokio::test]
async fn concurrent_acquire_outbound_node_distinct_when_tied() {
    // File DB allows multi-connection; :memory: pool is max_connections=1.
    let path = std::env::temp_dir().join(format!(
        "serpotter-node-acquire-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite:{}?mode=rwc", path.display());
    let db = serpotter_db::connect_and_migrate(&url)
        .await
        .expect("migrate");
    let a = db
        .insert_node("a.example", 8080, None, None)
        .await
        .unwrap();
    let b = db
        .insert_node("b.example", 8080, None, None)
        .await
        .unwrap();

    let db1 = db.clone();
    let db2 = db.clone();
    let (r1, r2) = tokio::join!(db1.acquire_outbound_node(), db2.acquire_outbound_node());
    let n1 = r1.expect("acquire1").expect("node1");
    let n2 = r2.expect("acquire2").expect("node2");

    // Atomic pick+bump: two concurrent acquires on tied inflight must not
    // double-bump the same least-id row; each node ends with inflight=1.
    assert_ne!(n1.id, n2.id, "must pick different nodes under concurrency");
    assert!(
        (n1.id == a.id && n2.id == b.id) || (n1.id == b.id && n2.id == a.id),
        "ids must be the two seeded nodes"
    );
    assert_eq!(n1.inflight, 1);
    assert_eq!(n2.inflight, 1);

    let nodes = db.list_nodes().await.unwrap();
    assert_eq!(nodes.iter().find(|n| n.id == a.id).unwrap().inflight, 1);
    assert_eq!(nodes.iter().find(|n| n.id == b.id).unwrap().inflight, 1);

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn node_fail_at_max_disables() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("fail.example", 1, None, None)
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 3).await.unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 3).await.unwrap();
    let mid = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(mid.consecutive_fails, 2);
    assert_eq!(mid.enabled, 1);

    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 3).await.unwrap();
    let dead = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(dead.consecutive_fails, 3);
    assert_eq!(dead.enabled, 0);
    assert_eq!(dead.inflight, 0);
    assert!(db.acquire_outbound_node().await.unwrap().is_none());
}

#[tokio::test]
async fn report_node_success_resets_fails_and_releases() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("ok.example", 1, None, None)
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 5).await.unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_success(n.id).await.unwrap();
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.consecutive_fails, 0);
    assert_eq!(row.inflight, 0);
    assert_eq!(row.enabled, 1);
}

#[tokio::test]
async fn zero_all_node_inflight_resets() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("z.example", 1, None, None)
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.zero_all_node_inflight().await.unwrap();
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.id, n.id);
    assert_eq!(row.inflight, 0);
}
