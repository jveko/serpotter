use super::*;
use serpotter_db::connect_and_migrate;
use std::sync::Arc;
use tokio::time::Duration as TokioDuration;

fn pool_with(db: Db, max_inflight: i64, timeout: Duration) -> KeyPool {
    KeyPool::with_config(db, max_inflight, timeout, serpotter_db::KEY_HOLD_TTL_SECS)
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

    let inflight: i64 =
        sqlx::query_scalar("SELECT inflight FROM api_keys WHERE id = ?")
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
