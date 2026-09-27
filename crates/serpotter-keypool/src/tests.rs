use super::*;
use serpotter_db::connect_and_migrate;
use std::sync::Arc;
use tokio::time::Duration as TokioDuration;

fn pool_with(db: Db, max_inflight: i64, timeout: Duration) -> KeyPool {
    KeyPool::with_config(
        db,
        max_inflight,
        timeout,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
}

fn pool_with_unknown(db: Db, max_inflight: i64, timeout: Duration, unknown: i64) -> KeyPool {
    KeyPool::with_config(
        db,
        max_inflight,
        timeout,
        serpotter_db::KEY_HOLD_TTL_SECS,
        unknown,
    )
}

#[tokio::test]
async fn empty_inventory_fail_fast() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    // Long timeout would hang if we waited; must fail immediately.
    let pool = pool_with(db, 3, Duration::from_secs(30));
    let start = Instant::now();
    let err = pool.acquire("tavily").await.unwrap_err();
    assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "empty inventory must not wait full acquire timeout"
    );
}

#[tokio::test]
async fn acquire_then_success() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-x").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.key, "tvly-x");
    pool.report_success(lease.identity()).await.unwrap();
}

#[tokio::test]
async fn shared_cap_three_then_wait_timeout() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-cap").await.unwrap();
    let pool = Arc::new(pool_with(db, 1, Duration::from_millis(200)));

    let first = pool.acquire("tavily").await.unwrap();
    let start = Instant::now();
    let err = pool.acquire("tavily").await.unwrap_err();
    assert!(matches!(err, KeyPoolError::AcquireTimeout(_)));
    assert!(
        start.elapsed() >= Duration::from_millis(150),
        "should wait until timeout when inventory exists but at cap"
    );
    // hold still live
    pool.release(first.identity()).await.unwrap();
}

#[tokio::test]
async fn shared_cap_waits_until_report() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-wait").await.unwrap();
    let pool = Arc::new(pool_with(db, 1, Duration::from_secs(5)));

    let first = pool.acquire("tavily").await.unwrap();
    let pool2 = Arc::clone(&pool);
    let waiter = tokio::spawn(async move { pool2.acquire("tavily").await });

    // Let waiter enter the wait path.
    tokio::time::sleep(TokioDuration::from_millis(50)).await;
    pool.report_success(first.identity()).await.unwrap();

    let second = waiter.await.unwrap().unwrap();
    assert_eq!(second.key, "tvly-wait");
    pool.report_success(second.identity()).await.unwrap();
}

#[tokio::test]
async fn shared_cap_waits_until_release() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-rel").await.unwrap();
    let pool = Arc::new(pool_with(db, 1, Duration::from_secs(5)));

    let first = pool.acquire("tavily").await.unwrap();
    let pool2 = Arc::clone(&pool);
    let waiter = tokio::spawn(async move { pool2.acquire("tavily").await });

    tokio::time::sleep(TokioDuration::from_millis(50)).await;
    pool.release(first.identity()).await.unwrap();

    let second = waiter.await.unwrap().unwrap();
    assert_eq!(second.id, first.id);
    pool.release(second.identity()).await.unwrap();
}

/// Regression: free+`notify_waiters` must not race past an unregistered `Notified`.
/// Without `enable()` before the recheck, a release between unlock and waiter
/// registration is lost (`notify_waiters` stores no permit) and acquire sleeps the
/// full timeout. Free immediately after spawn (no settle sleep/yield) so the race
/// window is open; assert second acquire finishes under 2s, not ~30s.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lost_wakeup_release_before_waiter_registers() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-race").await.unwrap();
    // Full default-scale timeout: hung waiter would burn ~30s without the fix.
    let pool = Arc::new(pool_with(db, 1, Duration::from_secs(30)));

    let first = pool.acquire("tavily").await.unwrap();
    let pool_w = Arc::clone(&pool);
    let start = Instant::now();
    let waiter = tokio::spawn(async move { pool_w.acquire("tavily").await });

    // Immediate free — no pre-sleep/yield settle (that would only cover already-parked waiters).
    pool.release(first.identity()).await.unwrap();

    let second = tokio::time::timeout(TokioDuration::from_secs(2), waiter)
        .await
        .expect("lost-wakeup: second acquire hung full timeout")
        .expect("waiter join")
        .expect("second acquire");
    assert_eq!(second.key, "tvly-race");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "second acquire must finish well under full 30s timeout, took {:?}",
        start.elapsed()
    );
    pool.release(second.identity()).await.unwrap();
}

#[tokio::test]
async fn release_does_not_increment_fails() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-nofail").await.unwrap();
    let pool = pool_with(db.clone(), 1, Duration::from_secs(5));

    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, k.id);
    pool.release(lease.identity()).await.unwrap();

    let row = db.get_api_key(k.id).await.unwrap().unwrap();
    assert_eq!(row.consecutive_fails, 0);
    assert_eq!(row.active, 1);
}

#[tokio::test]
async fn reclaim_after_hold_ttl() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-reclaim").await.unwrap();
    let pool = pool_with(db.clone(), 1, Duration::from_secs(5));

    let first = pool.acquire("tavily").await.unwrap();
    assert_eq!(first.id, k.id);

    // Force hold expiry so next shared acquire reclaims (full zero) then re-picks.
    sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '-1 seconds') WHERE api_key_id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();

    let second = pool.acquire("tavily").await.unwrap();
    assert_eq!(second.id, k.id);
    pool.release(second.identity()).await.unwrap();
}

/// C3a: refresh re-stamps `lease_until` for a still-held key — acquire →
/// force the lease into the past → refresh → the row lease moves forward
/// again (a long poll never lets its hold expire under it).
#[tokio::test]
async fn refresh_hold_re_stamps_lease_until() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-refresh").await.unwrap();
    let pool = pool_with(db.clone(), 1, Duration::from_secs(5));

    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, k.id);
    // Force the lease into the past — the next shared acquire would reclaim
    // it (full zero); a refresh must push it forward again.
    sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '-10 seconds') WHERE api_key_id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();

    assert!(
        pool.refresh_hold(lease.identity()).await.unwrap(),
        "a held id must report a live hold"
    );

    let fresh: i64 = sqlx::query_scalar(
        "SELECT CASE WHEN lease_until > datetime('now', '-5 seconds') THEN 1 ELSE 0 END \
         FROM api_keys WHERE id = ?",
    )
    .bind(k.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(fresh, 1, "refresh must move lease_until to now+TTL");
    // The hold is still live: release finishes normally.
    pool.release(lease.identity()).await.unwrap();
}

/// C3a: refreshing a released or absent id is a no-op — never an error or
/// panic (refresh is best-effort from the holder's side) — but reports
/// `false` (lease lost), and a released hold is never re-stamped.
#[tokio::test]
async fn refresh_hold_absent_or_released_is_noop() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db
        .insert_api_key("tavily", "tvly-refresh-noop")
        .await
        .unwrap();
    let pool = pool_with(db.clone(), 1, Duration::from_secs(5));

    // Absent id: Ok, no panic.
    assert!(
        !pool
            .refresh_hold(KeyLeaseRef {
                id: 9_999_999,
                token: 9_999_999
            })
            .await
            .unwrap(),
        "absent id must report no live hold"
    );

    // Acquire then release: refresh after release is Ok and must NOT leave a
    // stale lease behind (lease_until stays NULL after the last hold ends).
    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, k.id);
    pool.release(lease.identity()).await.unwrap();
    assert!(
        !pool.refresh_hold(lease.identity()).await.unwrap(),
        "released id must report lease lost"
    );
    let lease_until: Option<String> =
        sqlx::query_scalar("SELECT lease_until FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(lease_until, None, "released hold must not be re-stamped");
}

#[tokio::test]
async fn refresh_is_scoped_to_one_same_key_holder_and_stale_token_is_noop() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-two-holders")
        .await
        .unwrap();
    let pool = pool_with(db.clone(), 2, Duration::from_secs(5));
    let first = pool.acquire("tavily").await.unwrap();
    let second = pool.acquire("tavily").await.unwrap();
    assert_eq!(first.id, second.id);
    assert_ne!(first.token, second.token);

    sqlx::query(
        "UPDATE api_key_leases SET lease_until = datetime('now', '-10 seconds') WHERE token = ?",
    )
    .bind(first.token)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE api_key_leases SET lease_until = datetime('now', '-20 seconds') WHERE token = ?",
    )
    .bind(second.token)
    .execute(db.pool())
    .await
    .unwrap();

    assert!(pool.refresh_hold(second.identity()).await.unwrap());
    let (first_live, second_live): (i64, i64) = sqlx::query_as(
        "SELECT \
           CASE WHEN (SELECT lease_until FROM api_key_leases WHERE token = ?) > datetime('now') \
             THEN 1 ELSE 0 END, \
           CASE WHEN (SELECT lease_until FROM api_key_leases WHERE token = ?) > datetime('now') \
             THEN 1 ELSE 0 END",
    )
    .bind(first.token)
    .bind(second.token)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        (first_live, second_live),
        (0, 1),
        "refreshing one holder must extend only that child lease"
    );

    pool.release(second.identity()).await.unwrap();
    assert!(
        !pool.refresh_hold(second.identity()).await.unwrap(),
        "released child token is stale and must not be refreshed",
    );
    pool.release(first.identity()).await.unwrap();
}

#[tokio::test]
async fn report_exhausted_prefers_other_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let a = db.insert_api_key("tavily", "tvly-a").await.unwrap();
    let b = db.insert_api_key("tavily", "tvly-b").await.unwrap();
    db.set_api_key_credits(a.id, Some(10)).await.unwrap();
    db.set_api_key_credits(b.id, Some(10)).await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let marked = pool.acquire("tavily").await.unwrap();
    assert_eq!(marked.id, a.id);
    pool.report_exhausted(marked.identity(), "tavily", 60)
        .await
        .unwrap();
    // First pick: b (priority 1).
    let first = pool.acquire("tavily").await.unwrap();
    assert_eq!(first.id, b.id);
    pool.report_success(first.identity()).await.unwrap();
    // Pure LRU would prefer older a; CASE must still prefer healthy b.
    let second = pool.acquire("tavily").await.unwrap();
    assert_eq!(
        second.id, b.id,
        "credit priority must beat LRU favoring exhausted key"
    );
    pool.report_success(second.identity()).await.unwrap();
}

#[tokio::test]
async fn shared_cap_allows_multi_hold_same_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-multi").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));

    let a = pool.acquire("tavily").await.unwrap();
    let b = pool.acquire("tavily").await.unwrap();
    let c = pool.acquire("tavily").await.unwrap();
    assert_eq!(a.id, b.id);
    assert_eq!(b.id, c.id);

    pool.report_success(a.identity()).await.unwrap();
    pool.report_success(b.identity()).await.unwrap();
    pool.report_success(c.identity()).await.unwrap();
}

/// At-capacity multi-hold: expired shared lease full-zeros inflight (including
/// still-"live" holder slots). Next acquire may oversubscribe vs true HTTP count —
/// accepted personal-use; documents design cascade.
#[tokio::test]
async fn reclaim_at_capacity_may_oversubscribe() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-cascade").await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));

    let a = pool.acquire("tavily").await.unwrap();
    let b = pool.acquire("tavily").await.unwrap();
    let c = pool.acquire("tavily").await.unwrap();
    assert_eq!(a.id, k.id);
    assert_eq!(b.id, c.id);

    // Cap full: fourth would wait/timeout. Expire shared deadline → full zero reclaim.
    sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '-1 seconds') WHERE api_key_id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();

    // After reclaim cascade, capacity is free again (may oversubscribe vs a,b,c still "held"
    // by callers who forgot to report — design-accepted).
    let d = pool.acquire("tavily").await.unwrap();
    assert_eq!(d.id, k.id);

    // Late reports from a,b,c use max(0, inflight-1) and must not go negative.
    pool.release(a.identity()).await.unwrap();
    pool.release(b.identity()).await.unwrap();
    pool.release(c.identity()).await.unwrap();
    pool.release(d.identity()).await.unwrap();

    let inflight: i64 = sqlx::query_scalar("SELECT inflight FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(inflight, 0, "floor at 0 after late cascade reports");
}

/// After wait timeout, one final acquire attempt still runs (release without notify).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_final_recheck_sees_release() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-recheck").await.unwrap();
    let pool = std::sync::Arc::new(pool_with(db.clone(), 1, Duration::from_millis(50)));

    let hold = pool.acquire("tavily").await.unwrap();
    assert_eq!(hold.id, k.id);

    let pool2 = std::sync::Arc::clone(&pool);
    let waiter = tokio::spawn(async move { pool2.acquire("tavily").await });

    // Ensure waiter is parked on timeout path before we free capacity silently.
    tokio::time::sleep(Duration::from_millis(15)).await;
    // Free without notify_waiters so only post-timeout try_acquire_once can succeed.
    db.release_api_key_lease(hold.token).await.unwrap();

    let second = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("join")
        .expect("spawn")
        .expect("final recheck after timeout");
    assert_eq!(second.id, k.id);
    pool.release(second.identity()).await.unwrap();
}

#[tokio::test]
async fn report_banned_deletes_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("firecrawl", "fc-banned-1").await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));

    pool.revoke_key_row(k.id).await.unwrap();

    assert!(
        db.get_api_key(k.id).await.unwrap().is_none(),
        "banned key row must be hard-deleted"
    );
    let err = pool.acquire("firecrawl").await.unwrap_err();
    assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
}

#[tokio::test]
async fn report_banned_missing_id_is_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    // No row: delete is no-op success; must not error (multi-hold / double finish).
    pool.revoke_key_row(9_999_999).await.unwrap();
}

#[tokio::test]
async fn report_banned_after_acquire_removes_from_pool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let a = db.insert_api_key("firecrawl", "fc-a").await.unwrap();
    let b = db.insert_api_key("firecrawl", "fc-b").await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));

    let lease = pool.acquire("firecrawl").await.unwrap();
    // Whichever key was leased: ban it; the other must still acquire.
    let banned_id = lease.id;
    let other = if banned_id == a.id { b.id } else { a.id };
    pool.revoke_key_row(banned_id).await.unwrap();

    assert!(db.get_api_key(banned_id).await.unwrap().is_none());
    let next = pool.acquire("firecrawl").await.unwrap();
    assert_eq!(next.id, other);
    pool.report_success(next.identity()).await.unwrap();
}

#[tokio::test]
async fn acquire_prefers_higher_credits_when_idle() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let low = db.insert_api_key("tavily", "tvly-low").await.unwrap();
    db.set_api_key_credits(low.id, Some(10)).await.unwrap();
    let high = db.insert_api_key("tavily", "tvly-high").await.unwrap();
    db.set_api_key_credits(high.id, Some(100)).await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));

    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, high.id);
    pool.report_success(lease.identity()).await.unwrap();
}

#[tokio::test]
async fn report_success_soft_burns_via_pool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(3)).await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    pool.report_success(lease.identity()).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 2);
}

#[tokio::test]
async fn release_does_not_soft_burn() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-rel-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(7)).await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    pool.release(lease.identity()).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 7);
}

#[tokio::test]
async fn custom_unknown_weight_affects_null_vs_low_known() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let known = db.insert_api_key("tavily", "tvly-known").await.unwrap();
    db.set_api_key_credits(known.id, Some(5)).await.unwrap();
    let unknown = db.insert_api_key("tavily", "tvly-unk").await.unwrap();
    let _ = unknown;
    // unknown_weight=1 → known (5) wins; if weight were 1000, unknown would win
    let pool = pool_with_unknown(db, 3, Duration::from_secs(5), 1);
    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, known.id);
    pool.report_success(lease.identity()).await.unwrap();
}

// --- FU09: env parse failures warn (never silent) + TTL<timeout check --------

/// Test-only capture sink for WARN+ events (Arc-owned buffer, no leak).
#[derive(Clone, Default)]
struct CaptureSink(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for CaptureSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` with WARN+ events written into a captured buffer; returns the text.
fn capture_warns(f: impl FnOnce()) -> String {
    let sink = CaptureSink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false) // CI runners emit ANSI escapes; assertions need plain text
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, f);
    let guard = sink.0.lock();
    String::from_utf8_lossy(&guard).into_owned()
}

#[test]
fn invalid_env_i64_warns_and_applies_default() {
    let text = capture_warns(|| {
        assert_eq!(
            parse_env_i64_opt("KEY_MAX_INFLIGHT", Some("abc".into()), 3, 1, MAX_INFLIGHT),
            None,
            "unparseable value must yield no value so the caller falls back to the default"
        );
    });
    assert!(
        text.contains("KEY_MAX_INFLIGHT"),
        "warn must name the offending var: {text}"
    );
    assert!(
        text.contains("abc"),
        "warn must carry the raw offending value: {text}"
    );
    // The docs promise invalid values name the accepted range, not just the var.
    assert!(
        text.contains("min=1") && text.contains("max=1000"),
        "warn must carry the accepted range: {text}"
    );
}

#[test]
fn invalid_env_u64_warns_and_applies_default() {
    let text = capture_warns(|| {
        assert_eq!(
            parse_env_u64_opt(
                "KEY_ACQUIRE_TIMEOUT_SECS",
                Some("-5".into()),
                30,
                1,
                MAX_ACQUIRE_TIMEOUT_SECS,
            ),
            None,
            "negative value must yield no value so the caller falls back to the default"
        );
    });
    assert!(
        text.contains("KEY_ACQUIRE_TIMEOUT_SECS"),
        "warn must name the offending var: {text}"
    );
    assert!(
        text.contains("-5"),
        "warn must carry the raw offending value: {text}"
    );
    assert!(
        text.contains("min=1") && text.contains("max=3600"),
        "warn must carry the accepted range: {text}"
    );
}

/// A `KEY_*` var set to non-UTF-8 bytes is a misconfiguration, not an unset
/// one: `std::env::var` returns `VarError::NotUnicode`, and collapsing that
/// with `.ok()` would silently apply the default with no signal at all.
#[test]
fn non_unicode_key_env_warns_instead_of_looking_unset() {
    let _guard = ENV_LOCK.lock();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::env::set_var(
            "KEY_MAX_INFLIGHT",
            std::ffi::OsStr::from_bytes(&[0xff, 0xfe]),
        );
        let text = capture_warns(|| {
            assert_eq!(
                env_i64_ranged("KEY_MAX_INFLIGHT", DEFAULT_MAX_INFLIGHT, 1, MAX_INFLIGHT),
                DEFAULT_MAX_INFLIGHT,
                "non-UTF-8 must fall back to the default"
            );
        });
        std::env::remove_var("KEY_MAX_INFLIGHT");
        assert!(
            text.contains("KEY_MAX_INFLIGHT") && text.contains("not valid UTF-8"),
            "non-UTF-8 must warn loudly, not masquerade as unset: {text}"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = capture_warns(|| {});
    }
}

#[test]
fn valid_env_values_parse_without_warning() {
    let text = capture_warns(|| {
        assert_eq!(
            parse_env_i64_opt(
                "KEY_HOLD_TTL_SECS",
                Some("90".into()),
                1,
                1,
                MAX_HOLD_TTL_SECS
            ),
            Some(90)
        );
        assert_eq!(
            parse_env_u64_opt(
                "KEY_MAX_INFLIGHT",
                Some("7".into()),
                3,
                1,
                MAX_INFLIGHT as u64
            ),
            Some(7)
        );
        assert_eq!(
            parse_env_i64_opt(
                "KEY_UNKNOWN_CREDIT_WEIGHT",
                None,
                100,
                1,
                MAX_UNKNOWN_CREDIT_WEIGHT
            ),
            None
        );
    });
    assert!(
        text.is_empty(),
        "no warn expected for parseable/missing values: {text}"
    );
}

#[test]
fn hold_ttl_below_acquire_timeout_warns() {
    let text = capture_warns(|| {
        warn_if_hold_below_timeout(5, Duration::from_secs(30));
        // healthy pair must stay silent
        warn_if_hold_below_timeout(90, Duration::from_secs(30));
    });
    assert!(
        text.contains("KEY_HOLD_TTL_SECS < KEY_ACQUIRE_TIMEOUT_SECS"),
        "misconfiguration signal must be explicit: {text}"
    );
    assert!(
        text.contains("hold_ttl_secs=5"),
        "anchors the offending pair: {text}"
    );
    assert!(
        !text.contains("hold_ttl_secs=90"),
        "the healthy pair must not warn: {text}"
    );
}

#[test]
fn request_timeout_zero_uses_api_default_and_warns_for_default_hold() {
    let text = capture_warns(|| warn_if_hold_below_request_timeout(90, Some("0")));
    assert!(
        text.contains("KEY_HOLD_TTL_SECS < REQUEST_TIMEOUT_SECS"),
        "{text}"
    );
    assert!(text.contains("request_timeout_secs=120"), "{text}");
}

#[test]
fn request_timeout_guard_is_silent_when_hold_meets_effective_timeout() {
    let text = capture_warns(|| warn_if_hold_below_request_timeout(120, Some("120")));
    assert!(text.is_empty(), "healthy hold must not warn: {text}");
}

#[test]
fn request_timeout_normalization_matches_api_rules() {
    assert_eq!(effective_request_timeout_secs(None), 120);
    assert_eq!(effective_request_timeout_secs(Some("")), 120);
    assert_eq!(effective_request_timeout_secs(Some("0")), 120);
    assert_eq!(effective_request_timeout_secs(Some("invalid")), 120);
    assert_eq!(effective_request_timeout_secs(Some("86401")), 120);
    assert_eq!(effective_request_timeout_secs(Some("45")), 45);
    assert_eq!(effective_request_timeout_secs(Some("86400")), 86_400);
}

// --- T-poolsenv: every KEY_* tunable warns on out-of-range values -----------

/// Serializes process-env mutation so parallel tests never race set/remove.
static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The `KEY_*` knobs (plus `REQUEST_TIMEOUT_SECS`, which the pool also reads)
/// the matrix below mutates.
const ENV_KEYS: [&str; 5] = [
    "KEY_MAX_INFLIGHT",
    "KEY_ACQUIRE_TIMEOUT_SECS",
    "KEY_HOLD_TTL_SECS",
    "KEY_UNKNOWN_CREDIT_WEIGHT",
    "REQUEST_TIMEOUT_SECS",
];

/// Clear every knob, apply `set`, run the synchronous `f`, restore, and return
/// the captured WARN+ text.
///
/// `f` must stay synchronous: [`ENV_LOCK`] is a `parking_lot` guard that must
/// never be held across an await, and `KeyPool::new` reads env synchronously
/// anyway — so each test awaits its `Db` up front and only the env-sensitive
/// construction happens under the lock.
fn with_only_env<T>(set: &[(&str, &str)], f: impl FnOnce() -> T) -> String {
    let _guard = ENV_LOCK.lock();
    let saved: Vec<(String, Option<String>)> = ENV_KEYS
        .iter()
        .map(|k| ((*k).to_string(), std::env::var(k).ok()))
        .collect();
    for k in ENV_KEYS {
        std::env::remove_var(k);
    }
    for (k, v) in set {
        std::env::set_var(k, v);
    }
    let text = capture_warns(|| {
        f();
    });
    for (k, v) in saved {
        if let Some(v) = v {
            std::env::set_var(&k, v);
        } else {
            std::env::remove_var(&k);
        }
    }
    text
}

/// The compiled-default value the pool must fall back to for `key` when
/// `raw` is out of range, asserted through the public accessors.
fn assert_out_of_range_falls_back(db: &Db, key: &'static str, raw: &str) -> String {
    let text = with_only_env(&[(key, raw)], || {
        let pool = KeyPool::new(db.clone());
        match key {
            "KEY_MAX_INFLIGHT" => assert_eq!(
                pool.max_inflight(),
                DEFAULT_MAX_INFLIGHT,
                "{key}={raw} must fall back to the documented default, not a silent clamp"
            ),
            "KEY_UNKNOWN_CREDIT_WEIGHT" => assert_eq!(
                pool.unknown_credit_weight(),
                serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
                "{key}={raw} must fall back to the documented default, not a silent clamp"
            ),
            "KEY_HOLD_TTL_SECS" => assert_eq!(
                pool.hold_ttl_secs(),
                serpotter_db::KEY_HOLD_TTL_SECS,
                "{key}={raw} must fall back to the compiled default"
            ),
            "KEY_ACQUIRE_TIMEOUT_SECS" => assert_eq!(
                pool.acquire_timeout(),
                Duration::from_secs(DEFAULT_ACQUIRE_TIMEOUT_SECS),
                "{key}={raw} must fall back to the documented 30 s default"
            ),
            other => panic!("unknown knob {other}"),
        }
    });
    assert!(
        text.contains(key) && text.contains("out of range"),
        "{key}={raw} must warn loudly: {text}"
    );
    text
}

#[tokio::test]
async fn unset_key_env_uses_compiled_defaults_silently() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let text = with_only_env(&[], || {
        let pool = KeyPool::new(db.clone());
        assert_eq!(pool.max_inflight(), DEFAULT_MAX_INFLIGHT);
        assert_eq!(pool.acquire_timeout(), Duration::from_secs(30));
        assert_eq!(pool.hold_ttl_secs(), serpotter_db::KEY_HOLD_TTL_SECS);
        assert_eq!(
            pool.unknown_credit_weight(),
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT
        );
    });
    // The cross-knob `KEY_HOLD_TTL_SECS < REQUEST_TIMEOUT_SECS` warning is
    // expected here (default 90 s hold vs the 120 s effective request
    // timeout); what must be absent is any *range* warning.
    assert!(
        !text.contains("out of range") && !text.contains("not a valid"),
        "unset knobs must not trigger a range/parse warning: {text}"
    );
}

#[tokio::test]
async fn valid_key_env_values_win_without_warning() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let text = with_only_env(
        &[
            ("KEY_MAX_INFLIGHT", "7"),
            ("KEY_ACQUIRE_TIMEOUT_SECS", "45"),
            ("KEY_HOLD_TTL_SECS", "600"),
            ("KEY_UNKNOWN_CREDIT_WEIGHT", "250"),
        ],
        || {
            let pool = KeyPool::new(db.clone());
            assert_eq!(pool.max_inflight(), 7);
            assert_eq!(pool.acquire_timeout(), Duration::from_secs(45));
            assert_eq!(pool.hold_ttl_secs(), 600);
            assert_eq!(pool.unknown_credit_weight(), 250);
        },
    );
    assert!(
        text.is_empty(),
        "in-range values must be used as-is: {text}"
    );
}

#[tokio::test]
async fn out_of_range_key_knobs_warn_and_fall_back_to_default() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    // The audit's silent-clamp cases (0 / -5) plus the negative and oversize
    // ends of every documented range.
    for (key, raws) in [
        ("KEY_MAX_INFLIGHT", ["0", "-1", "1001"].as_slice()),
        (
            "KEY_UNKNOWN_CREDIT_WEIGHT",
            ["0", "-5", "1000001"].as_slice(),
        ),
        ("KEY_HOLD_TTL_SECS", ["0", "-1", "86401"].as_slice()),
        ("KEY_ACQUIRE_TIMEOUT_SECS", ["0", "3601"].as_slice()),
    ] {
        for raw in raws {
            assert_out_of_range_falls_back(&db, key, raw);
        }
    }
}

/// `KEY_ACQUIRE_TIMEOUT_SECS=0` used to parse cleanly and turn the pool into a
/// fail-immediately pool with no signal at all. The chosen rule is warn +
/// documented 30 s default, pinned here separately from the generic matrix
/// because the *consequence* (instant `KeyBusy` under load) is the real hazard.
#[tokio::test]
async fn acquire_timeout_zero_is_not_a_silent_fail_immediately_pool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let text = with_only_env(&[("KEY_ACQUIRE_TIMEOUT_SECS", "0")], || {
        let pool = KeyPool::new(db.clone());
        assert_eq!(
            pool.acquire_timeout(),
            Duration::from_secs(DEFAULT_ACQUIRE_TIMEOUT_SECS)
        );
    });
    assert!(
        text.contains("KEY_ACQUIRE_TIMEOUT_SECS") && text.contains("out of range"),
        "a fail-immediately pool must never be configured silently: {text}"
    );
}

// --- KeyTransition: what the report DID, not just that it ran ----------------

/// A failure below the disable threshold changes nothing an operator can act
/// on, so it must report `None` — only the fail@3 flip is `Disabled`. Reporting
/// every failure as `Disabled` would make the counter a retry count.
#[tokio::test]
async fn failure_reports_disabled_only_on_the_flip() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-t").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let mut transitions = Vec::new();
    for _ in 0..serpotter_db::MAX_CONSECUTIVE_FAILURES {
        let lease = pool.acquire("tavily").await.unwrap();
        transitions.push(
            pool.report_failure(lease.identity(), "tavily")
                .await
                .unwrap(),
        );
    }
    for (i, t) in transitions.iter().enumerate() {
        let expected = if i + 1 == serpotter_db::MAX_CONSECUTIVE_FAILURES as usize {
            KeyTransition::Disabled
        } else {
            KeyTransition::None
        };
        assert_eq!(*t, expected, "failure {} must report {expected:?}", i + 1);
    }
}

/// The demotion case: tracked credits going from 10 to 0 is a real state
/// change and is reported as `CreditsZeroed`.
#[tokio::test]
async fn exhausted_reports_credits_zeroed_on_a_real_change() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-c").await.unwrap();
    db.set_api_key_credits(k.id, Some(10)).await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(
        pool.report_exhausted(lease.identity(), "tavily", 60)
            .await
            .unwrap(),
        KeyTransition::CreditsZeroed
    );
}

/// A zero-credits key stays ACQUIRABLE, so it is legitimately reported over
/// and over. The second report must be `None`: the credits did not change, it
/// only re-wrote the existing 0. Counting it would make the counter a
/// "row has zero credits" reading, not a transition.
#[tokio::test]
async fn exhausted_on_already_zero_credits_reports_none() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-z").await.unwrap();
    db.set_api_key_credits(k.id, Some(0)).await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(
        pool.report_exhausted(lease.identity(), "tavily", 60)
            .await
            .unwrap(),
        KeyTransition::None,
        "rewriting an existing 0 is not a transition"
    );
}

/// An untracked key (Exa/xAI) has `credits_remaining IS NULL`, which the
/// exhausted write deliberately leaves NULL. That must never be read as a
/// zeroing.
#[tokio::test]
async fn exhausted_on_untracked_credits_reports_none() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("xai", "xai-n").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("xai").await.unwrap();
    assert_eq!(
        pool.report_exhausted(lease.identity(), "xai", 60)
            .await
            .unwrap(),
        KeyTransition::None
    );
}

/// A 402 zeroes even untracked credits (it is a proven account state), so it
/// is the one credit report that can produce `CreditsZeroed` from NULL.
#[tokio::test]
async fn payment_required_zeroes_untracked_credits() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("firecrawl", "fc-402").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("firecrawl").await.unwrap();
    assert_eq!(
        pool.report_payment_required(lease.identity(), "firecrawl")
            .await
            .unwrap(),
        KeyTransition::CreditsZeroed
    );
}

#[tokio::test]
async fn suspension_reports_suspended() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("exa", "exa-s").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("exa").await.unwrap();
    assert_eq!(
        pool.report_suspended(lease.identity(), "exa")
            .await
            .unwrap(),
        KeyTransition::Suspended
    );
}

/// The delete tier only counts a row that was ACTUALLY removed: revoking an
/// absent id (multi-hold / double finish) must not report a deletion.
#[tokio::test]
async fn revoke_reports_deleted_only_for_a_live_row() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("firecrawl", "fc-ban").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    assert_eq!(
        pool.revoke_key_row(k.id).await.unwrap(),
        KeyTransition::Deleted
    );
    assert_eq!(
        pool.revoke_key_row(k.id).await.unwrap(),
        KeyTransition::None,
        "a second revoke has no row to delete and must claim nothing"
    );
}

/// A lost lease (reclaimed, or a double finish) reports no transition: the
/// pool never wrote to any row, so claiming a state change would be a lie.
#[tokio::test]
async fn lost_lease_reports_no_transition() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-l").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    pool.release(lease.identity()).await.unwrap();
    assert_eq!(
        pool.report_failure(lease.identity(), "tavily")
            .await
            .unwrap(),
        KeyTransition::None
    );
    assert_eq!(
        pool.report_suspended(lease.identity(), "tavily")
            .await
            .unwrap(),
        KeyTransition::None
    );
    assert_eq!(
        pool.report_exhausted(lease.identity(), "tavily", 60)
            .await
            .unwrap(),
        KeyTransition::None
    );
}

/// Leases OVERLAP (`max_inflight` 3 by default), so when one leg disables a
/// key, the OTHER legs are still holding leases acquired while it was active.
/// They then finish against an already-inactive row. Gating `Disabled` on the
/// post state alone counted that single disable once per in-flight leg; this
/// pins that only the report which actually flipped the row reports it.
#[tokio::test]
async fn overlapping_leases_count_the_disable_once() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("tavily", "tvly-overlap").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));

    // Two holders acquired while the row was still active; the first report
    // below frees a slot, so each following failure can lease again.
    let b = pool.acquire("tavily").await.unwrap();
    let c = pool.acquire("tavily").await.unwrap();

    let mut transitions = Vec::new();
    for _ in 0..serpotter_db::MAX_CONSECUTIVE_FAILURES {
        let lease = pool.acquire("tavily").await.unwrap();
        transitions.push(
            pool.report_failure(lease.identity(), "tavily")
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        transitions,
        vec![
            KeyTransition::None,
            KeyTransition::None,
            KeyTransition::Disabled
        ],
        "only the report that trips the threshold reports the flip"
    );

    // `b` and `c` were leased while the row was active and are still held
    // through the flip. They now finish against an already-disabled row: each
    // must report None, or one key leaving rotation counts three times.
    for late in [b, c] {
        assert_eq!(
            pool.report_failure(late.identity(), "tavily")
                .await
                .unwrap(),
            KeyTransition::None,
            "a lease finishing after the flip must not re-count the disable"
        );
    }
}

/// The suspension SQL is UNCONDITIONAL (`active = 0,
/// disabled_reason = 'vendor_suspended'`), so without the PRE gate every leg
/// still holding a lease when the vendor-deactivation phrase lands would
/// report `Suspended` again: N in-flight legs, N increments, for ONE key
/// leaving rotation. Same overlap shape as the disable gate.
#[tokio::test]
async fn overlapping_leases_count_the_suspension_once() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_api_key("exa", "exa-overlap").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));

    // Two legs in flight on the same row, both leased while it was active.
    let first = pool.acquire("exa").await.unwrap();
    let late = pool.acquire("exa").await.unwrap();

    assert_eq!(
        pool.report_suspended(first.identity(), "exa")
            .await
            .unwrap(),
        KeyTransition::Suspended,
        "the leg that actually suspends the row reports it"
    );
    assert_eq!(
        pool.report_suspended(late.identity(), "exa").await.unwrap(),
        KeyTransition::None,
        "a second leg finishing on the already-suspended row must not re-count the suspension"
    );
}
