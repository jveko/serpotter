#[tokio::test]
async fn migrate_sets_schema_version_14() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let v = db.schema_version().await.expect("version");
    assert_eq!(v, serpotter_db::EXPECTED_SCHEMA_VERSION);
    assert_eq!(v, 14);
    db.ping().await.expect("ping");
}

#[tokio::test]
async fn reclaim_expired_node_holds_zeros_inflight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("reclaim.example", 1, None, None, "http")
        .await
        .unwrap();
    let row = db.acquire_outbound_node().await.unwrap().unwrap();
    assert_eq!(row.id, n.id);
    assert_eq!(row.inflight, 1);
    assert!(row.lease_until.is_some(), "acquire stamps lease_until");

    // Force expired lease.
    sqlx::query("UPDATE nodes SET lease_until = datetime('now', '-1 seconds') WHERE id = ?")
        .bind(n.id)
        .execute(db.pool())
        .await
        .unwrap();

    let n_reclaimed = db.reclaim_expired_node_holds().await.unwrap();
    assert_eq!(n_reclaimed, 1);
    let after = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(after.inflight, 0);
    assert_eq!(after.lease_until, None);
}

#[tokio::test]
async fn acquire_reclaims_expired_node_holds() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("acq-reclaim.example", 1, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    sqlx::query(
        "UPDATE nodes SET inflight = 5, lease_until = datetime('now', '-10 seconds') WHERE id = ?",
    )
    .bind(n.id)
    .execute(db.pool())
    .await
    .unwrap();

    let row = db.acquire_outbound_node().await.unwrap().unwrap();
    assert_eq!(row.id, n.id);
    // Reclaim zeroed then bump → inflight 1
    assert_eq!(row.inflight, 1);
    assert!(row.lease_until.is_some());
}

#[tokio::test]
async fn release_node_clears_lease_when_last_hold() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("release-lease.example", 1, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    let mid = db.get_node(n.id).await.unwrap().unwrap();
    assert!(mid.lease_until.is_some());
    db.release_node_inflight(n.id).await.unwrap();
    let after = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(after.inflight, 0);
    assert_eq!(after.lease_until, None);
}

#[tokio::test]
async fn settings_social_enabled_roundtrip() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    assert!(db.get_social_enabled().await.unwrap());
    db.set_social_enabled(false).await.unwrap();
    assert!(!db.get_social_enabled().await.unwrap());
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
    assert!(db
        .get_token_by_value("tok-missing")
        .await
        .unwrap()
        .is_none());
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
    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .expect("acq")
        .expect("some");
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
    assert!(db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT
        )
        .await
        .unwrap()
        .is_none());
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
async fn shared_acquire_prefers_positive_credits_over_zero() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    // Insert exhausted first (would win pure LRU if no priority)
    let zero = db.insert_api_key("tavily", "tvly-zero").await.unwrap();
    db.set_api_key_credits(zero.id, Some(0)).await.unwrap();
    let ok = db.insert_api_key("tavily", "tvly-ok").await.unwrap();
    // null credits = priority 1 (unknown); prefer over zero
    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("some");
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
    let credits: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(credits, Some(0), "exhausted must zero credits_remaining");
    // still acquirable as priority-2 fallback when it is the only key
    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("fallback");
    assert_eq!(acquired.id, k.id);
}

#[tokio::test]
async fn report_exhausted_preserves_null_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    // Providers without a usage API (Exa/xAI) start with NULL credits.
    let k = db.insert_api_key("xai", "xai-null-credits").await.unwrap();
    let before: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before, None, "fresh xai key has no credit snapshot");

    db.report_api_key_exhausted(k.id).await.unwrap();
    let after: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        after, None,
        "NULL credits must stay NULL so xai/exa are not demoted to the exhausted tier"
    );
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1, "exhausted must not hard-disable");

    // A tracked key still zeroes on exhausted (existing behavior).
    let t = db.insert_api_key("tavily", "tvly-tracked").await.unwrap();
    db.set_api_key_credits(t.id, Some(50)).await.unwrap();
    db.report_api_key_exhausted(t.id).await.unwrap();
    let rem: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(t.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rem, Some(0), "tracked credits must still zero on exhausted");
}

#[tokio::test]
async fn shared_acquire_only_exhausted_still_returns_key() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-only-zero").await.unwrap();
    db.set_api_key_credits(k.id, Some(0)).await.unwrap();
    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(acquired.id, k.id);
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
            None,
            None,
            None,
            None,
            None,
            None,
            None,
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
async fn request_log_purge_keeps_newest_on_created_at_tie() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    for i in 0..6 {
        db.insert_request_log(
            "/api/search",
            "POST",
            200,
            Some("tavily"),
            Some("tavily"),
            Some(10 + i),
            None,
            Some("hello"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("insert");
    }
    // Force an exact identical timestamp on every row (within the retention
    // window, captured once in Rust so the value is deterministic) so the `id`
    // tiebreak decides which window the cap keeps.
    let ts: String = sqlx::query_scalar("SELECT datetime('now', '-1 day')")
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE request_log SET created_at = ?")
        .bind(&ts)
        .execute(db.pool())
        .await
        .unwrap();

    let purged = db.purge_request_log(30, 4).await.expect("purge");
    assert_eq!(purged, 2);
    let rows = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 100,
            status: None,
            path_prefix: None,
            service: None,
            request_id: None,
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 4, "cap must keep exactly max_rows");
    assert_eq!(rows[0].id, 6, "newest id must be kept");
    assert_eq!(rows[1].id, 5, "second-newest id must be kept");
    assert_eq!(rows[2].id, 4, "third-newest id must be kept");
    assert_eq!(rows[3].id, 3, "fourth-newest id must be kept");
}

#[tokio::test]
async fn list_request_logs_status_filter() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    db.insert_request_log(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(10),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    db.insert_request_log(
        "/api/search",
        "POST",
        502,
        Some("firecrawl"),
        Some("firecrawl"),
        Some(99),
        Some("Upstream"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    db.insert_request_log(
        "/api/extract",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(12),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let rows = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 50,
            status: Some(502),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, 502);

    let all = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 50,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 3, "status=None must not filter");
}

#[tokio::test]
async fn acquire_reclaims_expired_key_holds() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-acq-reclaim")
        .await
        .unwrap();
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();
    // Stale hold: inflight pinned high, lease expired.
    sqlx::query(
        "UPDATE api_keys SET inflight = 5, lease_until = datetime('now', '-10 seconds') WHERE id = ?",
    )
    .bind(k.id)
    .execute(db.pool())
    .await
    .unwrap();

    let row = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.id, k.id);
    // Reclaim zeroed the stale inflight inside the acquire tx, then bumped to 1.
    assert_eq!(key_inflight(&db, k.id).await, 1);
    assert!(key_lease(&db, k.id).await.is_some());
}

#[tokio::test]
async fn request_log_v12_columns_and_path_prefix_filter() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    assert_eq!(db.schema_version().await.unwrap(), 14);

    db.insert_request_log(
        "/api/search",
        "POST",
        200,
        Some("tavily"),
        Some("tavily"),
        Some(12),
        None,
        Some("hello"),
        Some("req-1"),
        Some("local"),
        Some("single"),
        Some("tavily"),
        Some(1),
        Some(7),
        Some(3),
    )
    .await
    .unwrap();
    db.insert_request_log(
        "/api/extract",
        "POST",
        502,
        Some("firecrawl"),
        Some("firecrawl"),
        Some(99),
        Some("Upstream"),
        Some("https://x"),
        Some("req-2"),
        Some("ci"),
        None,
        Some("firecrawl"),
        Some(2),
        Some(8),
        None,
    )
    .await
    .unwrap();

    let rows = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 50,
            status: None,
            path_prefix: Some("/api/se".into()),
            service: None,
            request_id: None,
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_id.as_deref(), Some("req-1"));
    assert_eq!(rows[0].token_name.as_deref(), Some("local"));
    assert_eq!(rows[0].key_id, Some(7));
    assert_eq!(rows[0].node_id, Some(3));

    let by_id = db
        .list_request_logs(serpotter_db::RequestLogFilter {
            limit: 10,
            status: None,
            path_prefix: None,
            service: None,
            request_id: Some("req-2".into()),
        })
        .await
        .unwrap();
    assert_eq!(by_id.len(), 1);
    assert_eq!(by_id[0].status, 502);
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
    let got = db
        .get_admin_user_by_username("admin")
        .await
        .unwrap()
        .unwrap();
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

/// F58: the DELETE path of the 15m maintenance purge — expired sessions are
/// removed, fresh sessions survive (only the read-side expiry was covered by
/// `admin_user_and_session_roundtrip`).
#[tokio::test]
async fn purge_expired_admin_sessions_deletes_only_expired() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let user = db
        .insert_admin_user("admin", "$argon2id$placeholder")
        .await
        .unwrap();
    db.insert_admin_session("sess-expired-1", user.id, "2000-01-01 00:00:00")
        .await
        .unwrap();
    db.insert_admin_session("sess-expired-2", user.id, "1999-06-01 12:00:00")
        .await
        .unwrap();
    db.insert_admin_session("sess-fresh", user.id, "2099-01-01 00:00:00")
        .await
        .unwrap();

    let purged = db.purge_expired_admin_sessions().await.unwrap();
    assert_eq!(purged, 2, "exactly the two expired rows must be purged");

    assert!(db
        .get_valid_admin_session("sess-expired-1")
        .await
        .unwrap()
        .is_none());
    assert!(db
        .get_valid_admin_session("sess-expired-2")
        .await
        .unwrap()
        .is_none());
    let fresh = db
        .get_valid_admin_session("sess-fresh")
        .await
        .unwrap()
        .expect("fresh session must survive the purge");
    assert_eq!(fresh.token, "sess-fresh");

    // Idempotent second run: nothing left to purge.
    assert_eq!(db.purge_expired_admin_sessions().await.unwrap(), 0);
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
            .acquire_api_key_shared(
                "tavily",
                3,
                serpotter_db::KEY_HOLD_TTL_SECS,
                serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
            )
            .await
            .unwrap()
            .expect("hold");
        assert_eq!(got.id, k.id);
        assert_eq!(key_inflight(&db, k.id).await, i);
        assert!(key_lease(&db, k.id).await.is_some());
    }
    assert!(db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT
        )
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
        db.acquire_api_key_shared(
            "tavily",
            3,
            90,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
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
    db.acquire_api_key_shared(
        "tavily",
        3,
        90,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
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
async fn reclaim_at_capacity_may_oversubscribe() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-cascade").await.unwrap();
    // Fill soft cap (max_inflight=3).
    for _ in 0..3 {
        db.acquire_api_key_shared(
            "tavily",
            3,
            90,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("hold");
    }
    assert_eq!(key_inflight(&db, k.id).await, 3);
    assert!(
        db.acquire_api_key_shared(
            "tavily",
            3,
            90,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT
        )
        .await
        .unwrap()
        .is_none(),
        "at capacity"
    );

    // Expire shared deadline → full-zero reclaim zeros *all* holds (cascade).
    sqlx::query("UPDATE api_keys SET lease_until = datetime('now', '-1 seconds') WHERE id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();
    let n = db.reclaim_expired_key_holds().await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(key_inflight(&db, k.id).await, 0);

    // Next acquire succeeds (oversubscribe vs unreleased caller holds is accepted).
    let again = db
        .acquire_api_key_shared(
            "tavily",
            3,
            90,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("after cascade");
    assert_eq!(again.id, k.id);
    assert_eq!(key_inflight(&db, k.id).await, 1);

    // Late releases floor at 0.
    for _ in 0..5 {
        db.release_api_key_inflight(k.id).await.unwrap();
    }
    assert_eq!(key_inflight(&db, k.id).await, 0);
}

#[tokio::test]
async fn zero_all_key_inflight_clears_holds() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-zero").await.unwrap();
    db.acquire_api_key_shared(
        "tavily",
        5,
        90,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
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
        .insert_node("a.example", 8080, None, None, "http")
        .await
        .unwrap();
    let b = db
        .insert_node("b.example", 8080, None, None, "http")
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
    let path =
        std::env::temp_dir().join(format!("serpotter-node-acquire-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite:{}?mode=rwc", path.display());
    let db = serpotter_db::connect_and_migrate(&url)
        .await
        .expect("migrate");
    let a = db
        .insert_node("a.example", 8080, None, None, "http")
        .await
        .unwrap();
    let b = db
        .insert_node("b.example", 8080, None, None, "http")
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
        .insert_node("fail.example", 1, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 3, Some("connect reset"))
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 3, Some("tunnel timeout"))
        .await
        .unwrap();
    let mid = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(mid.consecutive_fails, 2);
    assert_eq!(mid.enabled, 1);
    assert_eq!(mid.last_error.as_deref(), Some("tunnel timeout"));

    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 3, Some("final fail"))
        .await
        .unwrap();
    let dead = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(dead.consecutive_fails, 3);
    assert_eq!(dead.enabled, 0);
    assert_eq!(dead.inflight, 0);
    assert_eq!(dead.last_error.as_deref(), Some("final fail"));
    assert!(db.acquire_outbound_node().await.unwrap().is_none());
}

#[tokio::test]
async fn node_fail_at_max_sets_disabled_at() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("fail-stamp.example", 1, None, None, "http")
        .await
        .unwrap();
    // Not yet at max: disabled_at stays NULL.
    db.report_node_failure(n.id, 3, Some("blip")).await.unwrap();
    let mid = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(mid.consecutive_fails, 1);
    assert_eq!(mid.enabled, 1);
    assert_eq!(mid.disabled_at, None, "not disabled yet → no stamp");

    db.report_node_failure(n.id, 3, Some("second"))
        .await
        .unwrap();
    db.report_node_failure(n.id, 3, Some("final"))
        .await
        .unwrap();
    let dead = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(dead.consecutive_fails, 3);
    assert_eq!(dead.enabled, 0);
    assert!(
        dead.disabled_at.is_some(),
        "disable at max_fails must stamp disabled_at"
    );
}

#[tokio::test]
async fn reenable_stale_nodes_flips_old_ones_only() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let old = db
        .insert_node("old.example", 1, None, None, "http")
        .await
        .unwrap();
    let fresh = db
        .insert_node("fresh.example", 2, None, None, "http")
        .await
        .unwrap();
    assert!(db.set_node_enabled(old.id, false).await.unwrap());
    assert!(db.set_node_enabled(fresh.id, false).await.unwrap());

    // Age the old node's disabled_at well beyond the recovery window.
    sqlx::query("UPDATE nodes SET disabled_at = datetime('now', '-48 hours') WHERE id = ?")
        .bind(old.id)
        .execute(db.pool())
        .await
        .unwrap();

    let n = db.reenable_stale_nodes(24).await.expect("reenable");
    assert_eq!(n, 1, "only the 48h-old node may re-enable");

    let old_row = db.get_node(old.id).await.unwrap().unwrap();
    assert_eq!(old_row.enabled, 1, "stale node re-enabled");
    assert_eq!(old_row.consecutive_fails, 0, "fails reset");
    assert_eq!(old_row.last_error, None, "last_error cleared");
    assert_eq!(old_row.disabled_at, None, "disabled_at cleared");

    let fresh_row = db.get_node(fresh.id).await.unwrap().unwrap();
    assert_eq!(fresh_row.enabled, 0, "freshly disabled node stays off");
    assert!(
        fresh_row.disabled_at.is_some(),
        "fresh disable retains its stamp"
    );
}

#[tokio::test]
async fn reenable_stale_nodes_skips_enabled_and_unstamped() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let on = db
        .insert_node("on.example", 1, None, None, "http")
        .await
        .unwrap();
    // Disabled but disabled_at NULL (pre-0014 row): must NOT re-enable.
    let unstamped = db
        .insert_node("unstamped.example", 2, None, None, "http")
        .await
        .unwrap();
    sqlx::query("UPDATE nodes SET enabled = 0, disabled_at = NULL WHERE id = ?")
        .bind(unstamped.id)
        .execute(db.pool())
        .await
        .unwrap();
    // Freshly disabled (stamp = now): must NOT re-enable yet.
    let fresh = db
        .insert_node("fresh.example", 3, None, None, "http")
        .await
        .unwrap();
    assert!(db.set_node_enabled(fresh.id, false).await.unwrap());

    let n = db.reenable_stale_nodes(1).await.expect("reenable");
    assert_eq!(n, 0, "no disabled+stamped+stale node qualifies");
    assert_eq!(db.get_node(on.id).await.unwrap().unwrap().enabled, 1);
    assert_eq!(
        db.get_node(unstamped.id).await.unwrap().unwrap().enabled,
        0,
        "disabled without disabled_at must not re-enable"
    );
    assert_eq!(
        db.get_node(fresh.id).await.unwrap().unwrap().enabled,
        0,
        "recently disabled must not re-enable"
    );
}

#[tokio::test]
async fn set_node_enabled_toggles_disabled_at() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("toggle.example", 1, None, None, "http")
        .await
        .unwrap();
    assert_eq!(db.get_node(n.id).await.unwrap().unwrap().disabled_at, None);

    // Admin disable stamps now.
    assert!(db.set_node_enabled(n.id, false).await.unwrap());
    let off = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(off.enabled, 0);
    assert!(
        off.disabled_at.is_some(),
        "admin disable must stamp disabled_at"
    );

    // Admin re-enable clears it (alongside fails/last_error).
    assert!(db.set_node_enabled(n.id, true).await.unwrap());
    let on = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(on.enabled, 1);
    assert_eq!(on.disabled_at, None, "re-enable must clear disabled_at");
    assert_eq!(on.consecutive_fails, 0);
    assert_eq!(on.last_error, None);
}

#[tokio::test]
async fn set_node_enabled_true_clears_fails_and_last_error() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("reenable.example", 1, None, None, "http")
        .await
        .unwrap();
    for msg in ["a", "b", "c"] {
        db.acquire_outbound_node().await.unwrap().unwrap();
        db.report_node_failure(n.id, 3, Some(msg)).await.unwrap();
    }
    let dead = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(dead.enabled, 0);
    assert_eq!(dead.consecutive_fails, 3);
    assert_eq!(dead.last_error.as_deref(), Some("c"));

    assert!(db.set_node_enabled(n.id, true).await.unwrap());
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.enabled, 1);
    assert_eq!(row.consecutive_fails, 0, "re-enable must reset fails");
    assert_eq!(row.last_error, None, "re-enable must clear last_error");

    // Disable alone must not wipe health history.
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 5, Some("kept")).await.unwrap();
    assert!(db.set_node_enabled(n.id, false).await.unwrap());
    let off = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(off.enabled, 0);
    assert_eq!(off.consecutive_fails, 1);
    assert_eq!(off.last_error.as_deref(), Some("kept"));
}

#[tokio::test]
async fn report_node_success_resets_fails_and_releases() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("ok.example", 1, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure(n.id, 5, Some("transient blip"))
        .await
        .unwrap();
    let after_fail = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(after_fail.last_error.as_deref(), Some("transient blip"));
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_success(n.id).await.unwrap();
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.consecutive_fails, 0);
    assert_eq!(row.inflight, 0);
    assert_eq!(row.enabled, 1);
    assert_eq!(row.last_error, None, "success must clear last_error");
}

#[tokio::test]
async fn zero_all_node_inflight_resets() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("z.example", 1, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    db.zero_all_node_inflight().await.unwrap();
    let row = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(row.id, n.id);
    assert_eq!(row.inflight, 0);
    assert_eq!(row.lease_until, None, "zero_all must clear lease_until");
}

#[tokio::test]
async fn shared_acquire_prefers_higher_credits_when_idle() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let low = db.insert_api_key("tavily", "tvly-low").await.unwrap();
    db.set_api_key_credits(low.id, Some(10)).await.unwrap();
    let high = db.insert_api_key("tavily", "tvly-high").await.unwrap();
    db.set_api_key_credits(high.id, Some(100)).await.unwrap();

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(
        acquired.id, high.id,
        "idle keys: higher credits_remaining must win"
    );
}

#[tokio::test]
async fn shared_acquire_load_damping_can_prefer_lower_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    // max_inflight=3: rich at inflight=2 → score (100*1000)/3 = 33333
    // poor at inflight=0 → score (50*1000)/1 = 50000 → poor wins
    let rich = db.insert_api_key("tavily", "tvly-rich").await.unwrap();
    db.set_api_key_credits(rich.id, Some(100)).await.unwrap();
    let poor = db.insert_api_key("tavily", "tvly-poor").await.unwrap();
    db.set_api_key_credits(poor.id, Some(50)).await.unwrap();

    sqlx::query("UPDATE api_keys SET inflight = 2 WHERE id = ?")
        .bind(rich.id)
        .execute(db.pool())
        .await
        .unwrap();

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(
        acquired.id, poor.id,
        "C/(inflight+1) must allow freer lower-credit key to beat loaded richer key"
    );
}

#[tokio::test]
async fn shared_acquire_null_before_exhausted_uses_mid_weight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let zero = db.insert_api_key("tavily", "tvly-zero").await.unwrap();
    db.set_api_key_credits(zero.id, Some(0)).await.unwrap();
    let unknown = db.insert_api_key("tavily", "tvly-null").await.unwrap();
    // credits_remaining stays NULL

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            /* unknown_weight */ 100,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(
        acquired.id, unknown.id,
        "NULL must beat exhausted tier even when inserted later"
    );
}

#[tokio::test]
async fn shared_acquire_high_known_beats_null_mid_weight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let unknown = db.insert_api_key("tavily", "tvly-null").await.unwrap();
    let _ = unknown;
    let high = db.insert_api_key("tavily", "tvly-high").await.unwrap();
    db.set_api_key_credits(high.id, Some(500)).await.unwrap();

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            100, // mid sentinel << 500
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(acquired.id, high.id);
}

#[tokio::test]
async fn report_success_soft_burns_non_null_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(5)).await.unwrap();
    // simulate one hold so success path is realistic
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();

    db.report_api_key_success(k.id).await.unwrap();

    let rem: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rem, Some(4));
}

#[tokio::test]
async fn report_success_leaves_null_credits_null() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("exa", "exa-null").await.unwrap();
    db.acquire_api_key_shared(
        "exa",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();

    db.report_api_key_success(k.id).await.unwrap();

    let rem: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rem, None);
}

#[tokio::test]
async fn report_success_never_negative_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-one").await.unwrap();
    db.set_api_key_credits(k.id, Some(1)).await.unwrap();
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();
    db.report_api_key_success(k.id).await.unwrap();
    // second success without re-acquire still floors at 0 (idempotent safety)
    db.report_api_key_success(k.id).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 0);
}

#[tokio::test]
async fn update_api_key_usage_overwrites_after_soft_burn() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-sync").await.unwrap();
    db.set_api_key_credits(k.id, Some(10)).await.unwrap();
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();
    db.report_api_key_success(k.id).await.unwrap(); // → 9
    db.update_api_key_usage(k.id, 42, 100).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 42, "sync must overwrite soft burn");
}

#[tokio::test]
async fn insert_node_protocol_round_trip() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .unwrap();
    for proto in ["http", "https", "socks5"] {
        let n = db
            .insert_node(&format!("{proto}.example"), 1, None, None, proto)
            .await
            .unwrap();
        assert_eq!(n.protocol, proto);
        let got = db.get_node(n.id).await.unwrap().unwrap();
        assert_eq!(got.protocol, proto);
    }
    let acq = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(
        matches!(acq.protocol.as_str(), "http" | "https" | "socks5"),
        "acquire RETURNING must include protocol"
    );
}
