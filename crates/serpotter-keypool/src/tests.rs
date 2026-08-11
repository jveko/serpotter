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
    pool.report_success(lease.id).await.unwrap();
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
    pool.release(first.id).await.unwrap();
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
    pool.report_success(first.id).await.unwrap();

    let second = waiter.await.unwrap().unwrap();
    assert_eq!(second.key, "tvly-wait");
    pool.report_success(second.id).await.unwrap();
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
    pool.release(first.id).await.unwrap();

    let second = waiter.await.unwrap().unwrap();
    assert_eq!(second.id, first.id);
    pool.release(second.id).await.unwrap();
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
    pool.release(first.id).await.unwrap();

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
    pool.release(second.id).await.unwrap();
}

#[tokio::test]
async fn release_does_not_increment_fails() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-nofail").await.unwrap();
    let pool = pool_with(db.clone(), 1, Duration::from_secs(5));

    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, k.id);
    pool.release(lease.id).await.unwrap();

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
    sqlx::query("UPDATE api_keys SET lease_until = datetime('now', '-1 seconds') WHERE id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();

    let second = pool.acquire("tavily").await.unwrap();
    assert_eq!(second.id, k.id);
    pool.release(second.id).await.unwrap();
}

#[tokio::test]
async fn report_exhausted_prefers_other_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let a = db.insert_api_key("tavily", "tvly-a").await.unwrap();
    let b = db.insert_api_key("tavily", "tvly-b").await.unwrap();
    db.set_api_key_credits(a.id, Some(10)).await.unwrap();
    db.set_api_key_credits(b.id, Some(10)).await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    pool.report_exhausted(a.id).await.unwrap();
    // First pick: b (priority 1).
    let first = pool.acquire("tavily").await.unwrap();
    assert_eq!(first.id, b.id);
    pool.report_success(first.id).await.unwrap();
    // Pure LRU would prefer older a; CASE must still prefer healthy b.
    let second = pool.acquire("tavily").await.unwrap();
    assert_eq!(
        second.id, b.id,
        "credit priority must beat LRU favoring exhausted key"
    );
    pool.report_success(second.id).await.unwrap();
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

    pool.report_success(a.id).await.unwrap();
    pool.report_success(b.id).await.unwrap();
    pool.report_success(c.id).await.unwrap();
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
    sqlx::query("UPDATE api_keys SET lease_until = datetime('now', '-1 seconds') WHERE id = ?")
        .bind(k.id)
        .execute(db.pool())
        .await
        .unwrap();

    // After reclaim cascade, capacity is free again (may oversubscribe vs a,b,c still "held"
    // by callers who forgot to report — design-accepted).
    let d = pool.acquire("tavily").await.unwrap();
    assert_eq!(d.id, k.id);

    // Late reports from a,b,c use max(0, inflight-1) and must not go negative.
    pool.release(a.id).await.unwrap();
    pool.release(b.id).await.unwrap();
    pool.release(c.id).await.unwrap();
    pool.release(d.id).await.unwrap();

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
    db.release_api_key_inflight(hold.id).await.unwrap();

    let second = tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("join")
        .expect("spawn")
        .expect("final recheck after timeout");
    assert_eq!(second.id, k.id);
    pool.release(second.id).await.unwrap();
}

#[tokio::test]
async fn report_banned_deletes_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("firecrawl", "fc-banned-1").await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));

    pool.report_banned(k.id).await.unwrap();

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
    pool.report_banned(9_999_999).await.unwrap();
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
    pool.report_banned(banned_id).await.unwrap();

    assert!(db.get_api_key(banned_id).await.unwrap().is_none());
    let next = pool.acquire("firecrawl").await.unwrap();
    assert_eq!(next.id, other);
    pool.report_success(next.id).await.unwrap();
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
    pool.report_success(lease.id).await.unwrap();
}

#[tokio::test]
async fn report_success_soft_burns_via_pool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(3)).await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    pool.report_success(lease.id).await.unwrap();
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
    pool.release(lease.id).await.unwrap();
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
    pool.report_success(lease.id).await.unwrap();
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
            parse_env_i64("KEY_MAX_INFLIGHT", Some("abc".into()), 3),
            3,
            "unparseable value must fall back to the default"
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
}

#[test]
fn invalid_env_u64_warns_and_applies_default() {
    let text = capture_warns(|| {
        assert_eq!(
            parse_env_u64("KEY_ACQUIRE_TIMEOUT_SECS", Some("-5".into()), 30),
            30,
            "negative value must fall back to the default"
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
}

#[test]
fn valid_env_values_parse_without_warning() {
    let text = capture_warns(|| {
        assert_eq!(parse_env_i64("KEY_HOLD_TTL_SECS", Some("90".into()), 1), 90);
        assert_eq!(parse_env_u64("KEY_MAX_INFLIGHT", Some("7".into()), 3), 7);
        assert_eq!(parse_env_i64("KEY_UNKNOWN_CREDIT_WEIGHT", None, 100), 100);
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
