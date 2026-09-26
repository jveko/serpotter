use sqlx::Row as _;
use std::sync::Arc;

#[tokio::test]
async fn migrate_sets_schema_version_20() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let v = db.schema_version().await.expect("version");
    assert_eq!(v, serpotter_db::EXPECTED_SCHEMA_VERSION);
    assert_eq!(v, 20);
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
    sqlx::query(
        "UPDATE node_leases SET lease_until = datetime('now', '-1 seconds') WHERE node_id = ?",
    )
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
        "UPDATE node_leases SET lease_until = datetime('now', '-10 seconds') WHERE node_id = ?",
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
    let acquired = db.acquire_outbound_node().await.unwrap().unwrap();
    let mid = db.get_node(n.id).await.unwrap().unwrap();
    assert!(mid.lease_until.is_some());
    db.release_node_lease(acquired.token).await.unwrap();
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
    assert_eq!(acquired.key.key, "tvly-test-key");

    db.note_key_health_failure(k.id).await.unwrap();
    db.note_key_health_failure(k.id).await.unwrap();
    let mid = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(mid.consecutive_fails, 2);
    assert_eq!(mid.active, 1);

    db.note_key_health_failure(k.id).await.unwrap();
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
    db.note_key_health_failure(k.id).await.unwrap();
    db.note_key_health_success(k.id).await.unwrap();
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
    assert_eq!(acquired.key.key, "tvly-ok");
}

#[tokio::test]
async fn report_exhausted_zeros_credits_keeps_active() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-e").await.unwrap();
    db.set_api_key_credits(k.id, Some(50)).await.unwrap();
    db.note_key_health_exhausted(k.id).await.unwrap();
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

    db.note_key_health_exhausted(k.id).await.unwrap();
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
    db.note_key_health_exhausted(t.id).await.unwrap();
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
    db.note_key_health_failure(k.id).await.unwrap();
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
    // A usage sync is a billing read, not a health signal: the pre-sync failure
    // must survive, or a still-broken key gets a clean bill of health every
    // cron tick and is re-enabled as soon as the window expires.
    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.consecutive_fails, 1, "credit sync must not reset fails");
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
        "UPDATE api_key_leases SET lease_until = datetime('now', '-10 seconds') WHERE api_key_id = ?",
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

/// Schema 18: a vendor deactivation must stay out of rotation. Before this,
/// `suspend_api_key` wrote a bare `active = 0` with no reason, so once a row sat
/// idle past `KEY_REENABLE_AFTER_HOURS` the maintenance cron brought the dead
/// account back and the next acquire would pay an acquire + an upstream `401` +
/// a WARN on every lap. Preventive: prod has not shown a repeat yet (see the
/// `reenable_stale_keys` doc).
#[tokio::test]
async fn vendor_suspension_is_not_revived_by_reenable() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-deactivated")
        .await
        .expect("insert");
    db.note_key_health_suspended(k.id).await.unwrap();
    db.set_api_key_last_used_at(k.id, Some("2000-01-01 00:00:00"))
        .await
        .unwrap();

    assert_eq!(db.reenable_stale_keys(24).await.expect("reenable"), 0);
    let row = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 0, "vendor-deactivated key must stay off");
    assert_eq!(row.disabled_reason.as_deref(), Some("vendor_suspended"));
}

/// The self-heal path must survive for the cohorts it was designed for: an
/// operator toggle. Migration 0018's backfill labels pre-existing inactive rows
/// the same way (`'manual'`, unless they look like vendor suspensions), so
/// revival keeps working for them; the marker is what the cron reads, and
/// revival must clear it rather than leave a row permanently stranded.
#[tokio::test]
async fn manual_disable_still_self_heals_and_clears_reason() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-manual")
        .await
        .expect("insert");
    db.set_api_key_active(k.id, false).await.unwrap();
    assert_eq!(
        db.get_api_key_admin(k.id)
            .await
            .unwrap()
            .unwrap()
            .disabled_reason
            .as_deref(),
        Some("manual"),
        "a manual toggle must not look like a vendor ban"
    );
    db.set_api_key_last_used_at(k.id, Some("2000-01-01 00:00:00"))
        .await
        .unwrap();

    assert_eq!(db.reenable_stale_keys(24).await.expect("reenable"), 1);
    let row = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(row.active, 1);
    assert_eq!(
        row.disabled_reason, None,
        "revival must clear the marker, not strand the row"
    );
}

/// A rotated secret is a new account: the old `'vendor_suspended'` marker must
/// not follow it, or a working key stays outside the re-enable cron forever.
#[tokio::test]
async fn key_rotation_clears_vendor_suspension() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-banned")
        .await
        .expect("insert");
    db.note_key_health_suspended(k.id).await.unwrap();
    db.set_api_key_last_used_at(k.id, Some("2000-01-01 00:00:00"))
        .await
        .unwrap();

    db.update_api_key(k.id, None, Some("tvly-fresh"))
        .await
        .unwrap();
    let row = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(
        row.disabled_reason, None,
        "rotation must clear the stale marker"
    );
    assert_eq!(row.active, 0, "rotation alone does not re-enable");
    // …but now the cron is allowed to bring it back.
    assert_eq!(db.reenable_stale_keys(24).await.expect("reenable"), 1);
}

/// A service reassignment points the row at a different vendor's account, so
/// the old vendor's suspension marker must not follow it either — same
/// stranding failure as a rotation, and easy to miss because the `key` did not
/// change.
#[tokio::test]
async fn service_reassignment_clears_vendor_suspension() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db
        .insert_api_key("tavily", "tvly-moved")
        .await
        .expect("insert");
    db.note_key_health_suspended(k.id).await.unwrap();
    db.set_api_key_last_used_at(k.id, Some("2000-01-01 00:00:00"))
        .await
        .unwrap();

    // service only — the secret is untouched.
    db.update_api_key(k.id, Some("firecrawl"), None)
        .await
        .unwrap();
    let row = db.get_api_key_admin(k.id).await.unwrap().unwrap();
    assert_eq!(row.service, "firecrawl");
    assert_eq!(
        row.disabled_reason, None,
        "a marker from the previous vendor must not strand the new account"
    );
    assert_eq!(db.reenable_stale_keys(24).await.expect("reenable"), 1);
}

/// `402` (out of money) and `429` (rate limited) must not share a credit
/// write. Exa/xAI rows are seeded `NULL` and are outside the credit-sync
/// allowlist, so a NULL-preserving report can never demote them: the key keeps
/// its unknown-credit mid-tier score and re-serves `402` on every lap.
/// Zeroing on a `429` instead would permanently sink a healthy account.
#[tokio::test]
async fn payment_required_zeroes_credits_that_exhausted_preserves() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let exhausted = db
        .insert_api_key("exa", "exa-rate-limited")
        .await
        .expect("insert");
    let broke = db
        .insert_api_key("exa", "exa-no-credits")
        .await
        .expect("insert");
    assert_eq!(
        db.get_api_key_admin(exhausted.id)
            .await
            .unwrap()
            .unwrap()
            .credits_remaining,
        None,
        "seeded keys start with unknown credits"
    );

    db.note_key_health_exhausted(exhausted.id).await.unwrap();
    db.note_key_health_payment_required(broke.id).await.unwrap();

    assert_eq!(
        db.get_api_key_admin(exhausted.id)
            .await
            .unwrap()
            .unwrap()
            .credits_remaining,
        None,
        "a 429 must not fabricate a zero"
    );
    assert_eq!(
        db.get_api_key_admin(broke.id)
            .await
            .unwrap()
            .unwrap()
            .credits_remaining,
        Some(0),
        "a 402 must sink the key to the exhausted-last tier"
    );
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
    let a = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    let b = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    let c = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 3);
    assert!(key_lease(&db, k.id).await.is_some());

    db.report_api_key_success_lease(a.token).await.unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 2);
    assert!(
        key_lease(&db, k.id).await.is_some(),
        "lease kept while holds remain"
    );
    db.release_api_key_lease(b.token).await.unwrap();
    assert_eq!(key_inflight(&db, k.id).await, 1);
    assert!(key_lease(&db, k.id).await.is_some());
    db.report_api_key_exhausted_lease(c.token).await.unwrap();
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

    sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '-1 seconds') WHERE api_key_id = ?")
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
    let mut stale_tokens = Vec::new();
    for _ in 0..3 {
        stale_tokens.push(
            db.acquire_api_key_shared("tavily", 3, 90, 100)
                .await
                .unwrap()
                .unwrap()
                .token,
        );
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
    sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '-1 seconds') WHERE api_key_id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();
    let n = db.reclaim_expired_key_holds().await.unwrap();
    assert_eq!(n, 3);
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

    for token in stale_tokens {
        db.release_api_key_lease(token).await.unwrap();
    }
    assert_eq!(key_inflight(&db, k.id).await, 1);
    let lease_is_live: i64 = sqlx::query_scalar(
        "SELECT CASE WHEN lease_until > datetime('now') THEN 1 ELSE 0 END FROM api_keys WHERE id = ?",
    )
    .bind(k.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(lease_is_live, 1, "new holder must retain a live lease");
    db.release_api_key_lease(again.token).await.unwrap();
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
    let _n = db
        .insert_node("fail.example", 1, None, None, "http")
        .await
        .unwrap();
    let lease = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(db
        .report_node_failure_lease(lease.token, 3, Some("connect reset"))
        .await
        .unwrap());
    let lease = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(db
        .report_node_failure_lease(lease.token, 3, Some("tunnel timeout"))
        .await
        .unwrap());
    let mid = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(mid.consecutive_fails, 2);
    assert_eq!(mid.enabled, 1);
    assert_eq!(mid.last_error.as_deref(), Some("tunnel timeout"));

    let lease = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(db
        .report_node_failure_lease(lease.token, 3, Some("final fail"))
        .await
        .unwrap());
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
    db.note_node_health_failure(n.id, 3, Some("blip"))
        .await
        .unwrap();
    let mid = db.get_node(n.id).await.unwrap().unwrap();
    assert_eq!(mid.consecutive_fails, 1);
    assert_eq!(mid.enabled, 1);
    assert_eq!(mid.disabled_at, None, "not disabled yet → no stamp");

    db.note_node_health_failure(n.id, 3, Some("second"))
        .await
        .unwrap();
    db.note_node_health_failure(n.id, 3, Some("final"))
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

/// `NODE_REENABLE_AFTER_HOURS=0` is not "cron disabled": fed straight into the
/// SQL it makes `disabled_at <= datetime('now')` true for EVERY disabled node,
/// so the next 15-minute tick re-enables a just-fail@max'd one (node backoff
/// silently off). Clamped to the shared 1h floor, a 30-minute-old disable is
/// NOT revived — the same contract the key side pins.
#[tokio::test]
async fn reenable_stale_nodes_zero_clamps_to_the_one_hour_floor() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let n = db
        .insert_node("floor-zero.example", 1, None, None, "http")
        .await
        .unwrap();
    assert!(db.set_node_enabled(n.id, false).await.unwrap());
    sqlx::query("UPDATE nodes SET disabled_at = datetime('now', '-30 minutes') WHERE id = ?")
        .bind(n.id)
        .execute(db.pool())
        .await
        .unwrap();

    assert_eq!(
        db.reenable_stale_nodes(0).await.expect("reenable"),
        0,
        "0 must not disable the backoff: a 30-minute-old disable stays off"
    );
    assert_eq!(db.get_node(n.id).await.unwrap().unwrap().enabled, 0);
}

/// A negative value used to form `datetime('now', '--1 hours')`, which SQLite
/// evaluates to NULL — a silent no-op matching nothing, which to an operator
/// reads as a healthy "no nodes to re-enable". It must now behave exactly like
/// the 1h floor: the stale node returns, the fresh one does not.
#[tokio::test]
async fn reenable_stale_nodes_treats_negative_as_the_floor_not_a_no_op() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let fresh = db
        .insert_node("floor-neg-fresh.example", 1, None, None, "http")
        .await
        .unwrap();
    let stale = db
        .insert_node("floor-neg-stale.example", 2, None, None, "http")
        .await
        .unwrap();
    for (id, offset) in [(fresh.id, "-30 minutes"), (stale.id, "-2 hours")] {
        assert!(db.set_node_enabled(id, false).await.unwrap());
        sqlx::query("UPDATE nodes SET disabled_at = datetime('now', ?) WHERE id = ?")
            .bind(offset)
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }

    assert_eq!(
        db.reenable_stale_nodes(-1).await.expect("reenable"),
        1,
        "negative hours must clamp to the floor, not silently match nothing"
    );
    assert_eq!(
        db.get_node(fresh.id).await.unwrap().unwrap().enabled,
        0,
        "a 30-minute-old disable is inside the 1h window"
    );
    assert_eq!(
        db.get_node(stale.id).await.unwrap().unwrap().enabled,
        1,
        "a 2-hour-old disable is outside it"
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
        db.note_node_health_failure(n.id, 3, Some(msg))
            .await
            .unwrap();
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
    let lease = db.acquire_outbound_node().await.unwrap().unwrap();
    db.report_node_failure_lease(lease.token, 5, Some("kept"))
        .await
        .unwrap();
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
    let _n = db
        .insert_node("ok.example", 1, None, None, "http")
        .await
        .unwrap();
    let lease = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(db
        .report_node_failure_lease(lease.token, 5, Some("transient blip"))
        .await
        .unwrap());
    let after_fail = db.list_nodes().await.unwrap().into_iter().next().unwrap();
    assert_eq!(after_fail.last_error.as_deref(), Some("transient blip"));
    let lease = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(db.report_node_success_lease(lease.token).await.unwrap());
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

    db.acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    db.set_api_key_active(poor.id, false).await.unwrap();
    db.acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    db.set_api_key_active(poor.id, true).await.unwrap();

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
    let hold = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(db.report_api_key_success_lease(hold.token).await.unwrap());

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
    let hold = db
        .acquire_api_key_shared("exa", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(db.report_api_key_success_lease(hold.token).await.unwrap());

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
    let hold = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(db.report_api_key_success_lease(hold.token).await.unwrap());
    // A stale double finish affects zero holders and cannot burn again.
    assert!(!db.report_api_key_success_lease(hold.token).await.unwrap());
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
    let hold = db
        .acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(db.report_api_key_success_lease(hold.token).await.unwrap());
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

// ---------------------------------------------------------------------------
// Hygiene migration 0020: FK pin, orphan pre-clean, DROP COLUMN, and the
// multi-connection key-pool cap / holder-reconcile behaviour that a
// `:memory:` pool (max_connections = 1) cannot reach.
// ---------------------------------------------------------------------------

/// Scratch directory for an on-disk database, removed (with its `-wal`/`-shm`
/// siblings) on drop. All tests in this binary share one process id, so the
/// per-test `tag` is what keeps the paths distinct.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("serpotter-db-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn db_url(&self) -> String {
        format!(
            "sqlite:{}?mode=rwc",
            self.0.join("serpotter.db").to_string_lossy()
        )
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // The whole directory goes, which is the only reliable way to also
        // reap the `-wal` / `-shm` files a WAL database leaves behind.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn migration_0020_drops_dead_api_keys_email_column() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let columns: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info('api_keys')")
        .fetch_all(db.pool())
        .await
        .expect("table info");
    assert!(
        !columns.iter().any(|c| c == "email"),
        "0020 must drop the never-read api_keys.email column, got {columns:?}"
    );
}

/// 0020 adds an index for exactly one scan, so its usefulness is PROVED with
/// EXPLAIN QUERY PLAN against the real statements rather than asserted from
/// intent. The counterfactual is measured too: the same statement is re-planned
/// inside a transaction that has dropped the index, so "this is a full table
/// SCAN without it" is demonstrated rather than claimed. The node cron needs
/// no new index — 0004's `idx_nodes_enabled` already serves it.
///
/// Scope: this runs on a freshly migrated (empty) database, so it proves the
/// index is USABLE, not that the cost model still prefers it at production
/// row counts.
#[tokio::test]
async fn reenable_cron_plans_are_indexed() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    // `hours` is BOUND, exactly as `reenable_stale_keys` binds it, so the plan
    // pinned here is the one the cron really runs. `detail` is read by name
    // off the fetched row: the first EXPLAIN column is the instruction id, so
    // a `query_scalar` would decode an integer.
    let row = sqlx::query(
        "EXPLAIN QUERY PLAN UPDATE api_keys SET active = 1, consecutive_fails = 0, \
                disabled_reason = NULL \
         WHERE active = 0 \
           AND disabled_reason IS NOT 'vendor_suspended' \
           AND last_used_at IS NOT NULL \
           AND last_used_at < datetime('now', '-' || ? || ' hours')",
    )
    .bind(24i64)
    .fetch_one(db.pool())
    .await
    .expect("plan reenable_stale_keys");
    let keys_plan: String = row.try_get("detail").expect("plan detail");
    assert!(
        keys_plan.contains("idx_api_keys_reenable"),
        "reenable_stale_keys must seek idx_api_keys_reenable, plan: {keys_plan}"
    );
    assert!(
        !keys_plan.contains("SCAN api_keys"),
        "must not fall back to a full table scan, plan: {keys_plan}"
    );

    // Counterfactual, demonstrated rather than claimed: SQLite DDL is
    // transactional, so dropping the index inside a transaction and rolling
    // back leaves the schema untouched. This is the plan 0020 is fixing.
    //
    // The `-- no index` suffix below is LOAD-BEARING, not cosmetic: sqlx caches
    // prepared statements per connection, so re-issuing the identical SQL text
    // would return the plan prepared against the pre-DROP schema. The comment
    // makes the cache key differ. Tidy it away and this silently regresses to
    // the stale plan.
    let mut tx = db.pool().begin().await.expect("begin");
    sqlx::query("DROP INDEX idx_api_keys_reenable")
        .execute(&mut *tx)
        .await
        .expect("drop index");
    let row = sqlx::query(
        "EXPLAIN QUERY PLAN UPDATE api_keys SET active = 1, consecutive_fails = 0, \
                disabled_reason = NULL \
         WHERE active = 0 \
           AND disabled_reason IS NOT 'vendor_suspended' \
           AND last_used_at IS NOT NULL \
           AND last_used_at < datetime('now', '-' || ? || ' hours') -- no index",
    )
    .bind(24i64)
    .fetch_one(&mut *tx)
    .await
    .expect("plan without the index");
    let without: String = row.try_get("detail").expect("plan detail");
    assert!(
        without.contains("SCAN api_keys") && !without.contains("idx_api_keys_reenable"),
        "without idx_api_keys_reenable the cron degrades to a full scan, \
         which is what 0020 exists to fix (a plan still naming the index means \
         the prepared-statement cache was reused, not that the planner kept it); \
         plan: {without}"
    );
    tx.rollback()
        .await
        .expect("rollback leaves the schema untouched");

    // `reenable_stale_nodes` (nodes.rs) needs NO new index: 0004's plain
    // `idx_nodes_enabled` already plans it as a SEARCH on `enabled = 0`. This
    // assertion is what justified dropping a second `nodes(disabled_at)` index
    // from 0020 — the planner would never have chosen it, so it was pure write
    // churn. If a future migration changes that, this test says why.
    let row = sqlx::query(
        "EXPLAIN QUERY PLAN UPDATE nodes SET enabled = 1, consecutive_fails = 0, last_error = NULL, disabled_at = NULL \
         WHERE enabled = 0 AND disabled_at IS NOT NULL \
           AND disabled_at <= datetime('now', '-' || ? || ' hours')",
    )
    .bind(24i64)
    .fetch_one(db.pool())
    .await
    .expect("plan reenable_stale_nodes");
    let nodes_plan: String = row.try_get("detail").expect("plan detail");
    assert!(
        nodes_plan.contains("idx_nodes_enabled"),
        "reenable_stale_nodes must keep using 0004's idx_nodes_enabled, plan: {nodes_plan}"
    );
    assert!(
        !nodes_plan.contains("SCAN nodes"),
        "must not fall back to a full table scan, plan: {nodes_plan}"
    );
}

/// `connect_and_migrate` must PIN `foreign_keys=ON` and the busy timeout.
/// sqlx 0.9 already defaults both this way, so the orphan-insert rejection
/// below also holds on an unpatched build — the assertions that actually
/// differ are the pragma read-backs, read from five SIMULTANEOUSLY checked
/// out connections (a sequential loop would just reuse one pooled connection).
#[tokio::test]
async fn connect_and_migrate_pins_fk_and_busy_timeout_on_every_connection() {
    let dir = TempDir::new("fk");
    let db = serpotter_db::connect_and_migrate(&dir.db_url())
        .await
        .expect("migrate on-disk");

    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(db.pool().acquire().await.expect("checkout connection"));
    }
    // Reaching the loop below at all is the real check: five SIMULTANEOUS
    // checkouts only complete if the on-disk pool really opened 5 connections.
    for conn in &mut held {
        let on: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut **conn)
            .await
            .expect("pragma");
        assert_eq!(on, 1, "PRAGMA foreign_keys must be ON on every connection");
        let timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&mut **conn)
            .await
            .expect("pragma");
        assert_eq!(
            timeout,
            (serpotter_db::SQLITE_BUSY_TIMEOUT_SECS * 1000) as i64,
            "busy_timeout must be pinned to SQLITE_BUSY_TIMEOUT_SECS"
        );
    }
    drop(held);

    let err = sqlx::query(
        "INSERT INTO admin_sessions (token, user_id, expires_at) VALUES ('orphan', 999999, '2099-01-01 00:00:00')",
    )
    .execute(db.pool())
    .await
    .expect_err("orphan admin_sessions.user_id must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("FOREIGN KEY") || msg.contains("foreign key"),
        "expected an FK violation, got: {msg}"
    );

    // The lease holder tables are enforced the same way, and the delete paths
    // that clear holders before removing a parent stay legal.
    let user = db
        .insert_admin_user("fk-admin", "hash")
        .await
        .expect("user");
    db.insert_admin_session("good", user.id, "2099-01-01 00:00:00")
        .await
        .expect("valid session");
    let k = db.insert_api_key("tavily", "tvly-fk-0001").await.unwrap();
    db.acquire_api_key_shared("tavily", 3, 90, 100)
        .await
        .unwrap()
        .expect("acquire");
    let n = db
        .insert_node("fk.example", 1, None, None, "http")
        .await
        .unwrap();
    db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(
        db.delete_node(n.id).await.unwrap(),
        "explicit holder cleanup keeps the node delete legal under FKs"
    );
    assert!(db.delete_api_key(k.id).await.unwrap());
}

/// 0020's orphan DELETEs are a hygiene net for a database written by a
/// non-sqlx client (the sqlite3 CLI, an operator script, a restored backup) —
/// sqlx itself would never have let the row in. Seed such a legacy file with an
/// explicitly FK-OFF connection, then boot it the way a process does: 0020
/// must apply, clear the dangling row, and leave the valid data alone.
#[tokio::test]
async fn migration_0020_cleans_orphan_sessions_from_a_legacy_database() {
    let dir = TempDir::new("orphan");
    let url = dir.db_url();

    let migrator = sqlx::migrate!("./migrations");
    {
        use sqlx::Connection as _;
        use std::str::FromStr as _;
        // Explicitly unconstrained, which is what a non-sqlx writer looks like
        // (the sqlx default is foreign_keys=ON, so `connect_with` on plain
        // options would refuse the orphan insert).
        let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&url)
            .expect("parse url")
            .create_if_missing(true)
            .foreign_keys(false);
        let mut conn = sqlx::SqliteConnection::connect_with(&opts)
            .await
            .expect("open legacy database");
        sqlx::query("PRAGMA journal_mode = WAL")
            .execute(&mut conn)
            .await
            .unwrap();
        // `impl Acquire for &mut SqliteConnection` exists in sqlx 0.9, so
        // `run_to` takes the connection directly.
        migrator
            .run_to(19, &mut conn)
            .await
            .expect("migrate the legacy database to 19");
        sqlx::query("INSERT INTO admin_users (username, password_hash) VALUES ('legacy', 'h')")
            .execute(&mut conn)
            .await
            .unwrap();
        // All THREE dangling-row shapes 0020 cleans, not just sessions: a
        // mutation that removes any one of its DELETEs must fail this test.
        // The lease child tables are seeded straight from a missing parent id
        // (their `token` is an explicit INTEGER PK, so the caller sets it).
        sqlx::query("INSERT INTO admin_sessions (token, user_id, expires_at) VALUES ('orphan', 4242, '2099-01-01 00:00:00')")
            .execute(&mut conn)
            .await
            .expect("FK is off on this connection, so the orphan is accepted");
        sqlx::query("INSERT INTO api_key_leases (token, api_key_id, lease_until) VALUES (9001, 999999, '2099-01-01 00:00:00')")
            .execute(&mut conn)
            .await
            .expect("orphan key lease is accepted with FKs off");
        sqlx::query("INSERT INTO node_leases (token, node_id, lease_until) VALUES (9002, 999999, '2099-01-01 00:00:00')")
            .execute(&mut conn)
            .await
            .expect("orphan node lease is accepted with FKs off");
    }

    let db = serpotter_db::connect_and_migrate(&url)
        .await
        .expect("an existing database with an orphan must still boot");
    assert_eq!(db.schema_version().await.unwrap(), 20);
    // Every one of 0020's three cleanup statements is pinned here, so
    // deleting any single one of them from the migration fails this test.
    for (label, sql) in [
        (
            "admin_sessions",
            "SELECT COUNT(*) FROM admin_sessions WHERE user_id NOT IN (SELECT id FROM admin_users)",
        ),
        (
            "api_key_leases",
            "SELECT COUNT(*) FROM api_key_leases WHERE api_key_id NOT IN (SELECT id FROM api_keys)",
        ),
        (
            "node_leases",
            "SELECT COUNT(*) FROM node_leases WHERE node_id NOT IN (SELECT id FROM nodes)",
        ),
    ] {
        let orphans: i64 = sqlx::query_scalar(sql)
            .fetch_one(db.pool())
            .await
            .unwrap_or_else(|e| panic!("count orphan {label}: {e}"));
        assert_eq!(orphans, 0, "0020 must delete pre-existing orphan {label}");
    }
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(kept, 1, "the clean legacy data must survive the migration");
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM admin_sessions")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(sessions, 0, "the orphan was the only session row");
    // And enforcement is live on the migrated database.
    assert!(db
        .insert_admin_session("still-orphan", 999999, "2099-01-01 00:00:00")
        .await
        .is_err());
}

/// First on-disk, multi-connection coverage of the KEY pool's shared cap and
/// holder reconciliation. `concurrent_acquire_outbound_node_distinct_when_tied`
/// above already runs on a file DB, but it checks node pick; nothing covered
/// "N racers against the key cap, then every holder released". The
/// acquire transaction writes first (the reclaim DELETE), so contention is on
/// the write lock and is retryable under `busy_timeout`; holds are kept as
/// lease ROWS, never as checked-out connections, so 12 racers cannot starve a
/// 5-connection pool.
///
/// The barriers make the outcome a function of the cap arithmetic, not of
/// timing; the timeouts exist only so a dead racer fails loudly instead of
/// hanging CI.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn on_disk_wal_concurrent_key_acquires_never_exceed_the_cap() {
    const CAP: i64 = 3;
    const KEYS: usize = 4;
    const RACERS: usize = 12;
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

    let dir = TempDir::new("wal");
    let db = serpotter_db::connect_and_migrate(&dir.db_url())
        .await
        .expect("migrate on-disk");
    let jm: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(jm.to_lowercase(), "wal", "on-disk DBs must run in WAL mode");
    for i in 0..KEYS {
        db.insert_api_key("tavily", &format!("tvly-wal-{i:04}"))
            .await
            .unwrap();
    }

    // Two-phase gate built from BARRIERS, not a `Notify`. A `Notified` only
    // joins the wait list when it is first polled, so racing `notify_waiters`
    // against a late poller is a missed-wakeup race (`notify_waiters` stores
    // no permit) — and `enable()`-style registration tricks are exactly the
    // kind of subtlety that silently reopens the hole. A barrier cannot
    // release early, so the window does not exist.
    //
    // Phase 1 `all_done`: every racer has an outcome, so the counters are
    // settled. Phase 2 `release_gate`: main re-opens the pool and releases
    // everyone. EVERY racer passes phase 2 before it can return — including
    // one that was refused a lease — otherwise the barrier would never
    // complete.
    let ready = Arc::new(tokio::sync::Barrier::new(RACERS));
    let all_done = Arc::new(tokio::sync::Barrier::new(RACERS + 1));
    let release_gate = Arc::new(tokio::sync::Barrier::new(RACERS + 1));
    // Racers never panic: a panic would skip the barriers and leave the rest
    // of the test waiting out DEADLINE before the real message surfaced.
    // Instead the failure is recorded and every racer still crosses every
    // barrier, so main can report it immediately with its original text.
    // (`Barrier::wait` is not cancel-safe, so abandoning a racer mid-wait is
    // not an option either.)
    let failure: Arc<tokio::sync::Mutex<Option<String>>> = Arc::new(tokio::sync::Mutex::new(None));
    let mut tasks = Vec::new();
    for _ in 0..RACERS {
        let db = db.clone();
        let ready = ready.clone();
        let all_done = all_done.clone();
        let release_gate = release_gate.clone();
        let failure = failure.clone();
        tasks.push(tokio::spawn(async move {
            // Start gate: parks every racer so they hit the write lock
            // together, which is the contention this test means to document.
            // The cap assertions below are arithmetic over the stored
            // counters, so they hold whether or not the racers overlap in
            // practice; the gate records the intent rather than guaranteeing
            // a reproducible interleaving (a racer mutated to skip it still
            // passes, since spawn scheduling is near-simultaneous anyway).
            ready.wait().await;
            let acquired = match db.acquire_api_key_shared("tavily", CAP, 90, 100).await {
                Ok(lease) => lease,
                Err(e) => {
                    *failure.lock().await =
                        Some(format!("acquire failed under WAL contention: {e}"));
                    None
                }
            };
            all_done.wait().await;
            // No early return before the gate: a refused racer must still
            // count as present or main's barrier wait never completes.
            release_gate.wait().await;
            let lease = acquired?;
            db.release_api_key_lease(lease.token)
                .await
                .expect("release");
            Some(lease.id)
        }));
    }

    // Read the stored counters only once every racer has an outcome. Polling
    // while the writes are still in flight is inherently racy (a transaction
    // can bump `inflight` before inserting its holder row), and the per-row
    // check is what the cap contract is really about: no key ever holds more
    // than CAP leases.
    tokio::time::timeout(DEADLINE, all_done.wait())
        .await
        .expect("every racer must report within the deadline");

    let peak: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(inflight), 0) FROM api_keys")
        .fetch_one(db.pool())
        .await
        .expect("read peak per-key inflight");
    let total: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(inflight), 0) FROM api_keys")
        .fetch_one(db.pool())
        .await
        .expect("read total inflight");
    let holders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_key_leases")
        .fetch_one(db.pool())
        .await
        .expect("count holders");
    let per_key: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT ak.id, ak.inflight, (SELECT COUNT(*) FROM api_key_leases l WHERE l.api_key_id = ak.id) \
         FROM api_keys ak ORDER BY ak.id",
    )
    .fetch_all(db.pool())
    .await
    .expect("per-key inflight");

    // Surface a racer failure with its ORIGINAL message before the cap
    // assertions, so a broken acquire reads as itself rather than as a
    // downstream `total` mismatch. The gate is still opened so the racers are
    // not left blocked when the test unwinds.
    // The guard must not be held across the barrier wait below, so the value
    // is copied out in its own scope.
    let racer_failure = { failure.lock().await.clone() };
    if let Some(msg) = racer_failure {
        release_gate.wait().await;
        panic!("{msg}");
    }

    assert_eq!(
        holders, total,
        "stored inflight must equal the live holder rows: {per_key:?}"
    );
    for (id, inflight, rows) in &per_key {
        assert!(
            *inflight <= CAP,
            "key {id} holds {inflight} leases, over the cap {CAP}"
        );
        assert_eq!(*inflight, *rows, "key {id} counter drifted: {per_key:?}");
    }
    assert_eq!(
        total,
        (CAP * KEYS as i64).min(RACERS as i64),
        "every key must be filled to the cap by the race: {per_key:?}"
    );
    assert_eq!(peak, CAP, "the race must actually park holds at the cap");

    // Phase 2: every holder releases at once. Opening the gate (rather than
    // signalling) means the racers are provably all parked before any release
    // starts, so the writers really do contend.
    //
    // If an assertion above panics before this point, the racers stay blocked
    // on the gate until the runtime tears the test down — the reported failure
    // is still that assertion, and the DEADLINE below is the backstop for the
    // healthy path.
    tokio::time::timeout(DEADLINE, release_gate.wait())
        .await
        .expect("every racer must reach the release gate within the deadline");
    let join_all = async {
        let mut granted = 0i64;
        for task in tasks {
            if task.await.expect("racer task").is_some() {
                granted += 1;
            }
        }
        granted
    };
    // `granted` is the racer-side view of the same fact `total == CAP * KEYS`
    // asserted above (every racer got a lease); it is kept as the check that
    // each task actually ran to completion rather than being dropped.
    let granted = tokio::time::timeout(DEADLINE, join_all)
        .await
        .expect("every release must finish within the deadline");
    assert_eq!(granted, RACERS as i64, "every racer task must have run");

    let leases_left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_key_leases")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(leases_left, 0, "every holder row must be released");
    let inflight: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(inflight), 0) FROM api_keys")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(inflight, 0, "api_keys.inflight must reconcile to 0");
}
