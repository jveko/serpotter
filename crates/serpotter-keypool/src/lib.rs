//! Lean in-process key pool over sqlx `api_keys`.
//!
//! Shared soft cap (`max_inflight` per key) with wait/notify when inventory exists
//! but all keys are at cap. Durable holds live in SQLite (`inflight` + `lease_until`).
//! **Single-process only** — mutex + Notify are not multi-instance safe.

use std::pin::pin;
use std::time::{Duration, Instant};

use serpotter_db::{ApiKeyRow, Db, DbError};
use thiserror::Error;
use tokio::sync::{Mutex, Notify};

pub const MAX_BATCH: usize = 10;

const DEFAULT_MAX_INFLIGHT: i64 = 3;
const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Error)]
pub enum KeyPoolError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("no healthy key for service {0}")]
    NoHealthyKey(String),
}

#[derive(Clone, Debug)]
pub struct LeasedKey {
    pub id: i64,
    pub service: String,
    pub key: String,
}

pub struct KeyPool {
    db: Db,
    /// Serializes reclaim+pick+bump so concurrent acquires do not stampede the same row
    /// before optimistic inflight updates land.
    lock: Mutex<()>,
    notify: Notify,
    max_inflight: i64,
    acquire_timeout: Duration,
    hold_ttl_secs: i64,
}

impl KeyPool {
    /// Build from env: `KEY_MAX_INFLIGHT` (3), `KEY_ACQUIRE_TIMEOUT_SECS` (30),
    /// `KEY_HOLD_TTL_SECS` (90 / `serpotter_db::KEY_HOLD_TTL_SECS`).
    pub fn new(db: Db) -> Self {
        Self::with_config(
            db,
            env_i64("KEY_MAX_INFLIGHT", DEFAULT_MAX_INFLIGHT),
            Duration::from_secs(env_u64(
                "KEY_ACQUIRE_TIMEOUT_SECS",
                DEFAULT_ACQUIRE_TIMEOUT_SECS,
            )),
            env_i64("KEY_HOLD_TTL_SECS", serpotter_db::KEY_HOLD_TTL_SECS),
        )
    }

    /// Explicit limits (tests and callers that cannot rely on process env).
    pub fn with_config(
        db: Db,
        max_inflight: i64,
        acquire_timeout: Duration,
        hold_ttl_secs: i64,
    ) -> Self {
        Self {
            db,
            lock: Mutex::new(()),
            notify: Notify::new(),
            max_inflight: max_inflight.max(1),
            acquire_timeout,
            hold_ttl_secs: hold_ttl_secs.max(1),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub fn max_inflight(&self) -> i64 {
        self.max_inflight
    }

    pub fn acquire_timeout(&self) -> Duration {
        self.acquire_timeout
    }

    pub fn hold_ttl_secs(&self) -> i64 {
        self.hold_ttl_secs
    }
    /// Shared-cap acquire: wait only when active keys exist but all are at `max_inflight`.
    /// Empty / inactive inventory → fail-fast `NoHealthyKey` (no full timeout wait).
    ///
    /// `Notified` is pinned before the critical section and `enable()`d **while still holding**
    /// the mutex, then the lock is dropped before await — so `notify_waiters` cannot race the
    /// gap between "decide to wait" and "registered as waiter" (notify_waiters has no permit).
    pub async fn acquire(&self, service: &str) -> Result<LeasedKey, KeyPoolError> {
        let deadline = Instant::now() + self.acquire_timeout;
        loop {
            let mut notified = pin!(self.notify.notified());
            {
                let _g = self.lock.lock().await;
                if let Some(row) = self
                    .db
                    .acquire_api_key_shared(service, self.max_inflight, self.hold_ttl_secs)
                    .await?
                {
                    return Ok(to_lease(row));
                }
                if self.db.count_active_keys(service).await? == 0 {
                    return Err(KeyPoolError::NoHealthyKey(service.to_string()));
                }
                // Register under the lock; drop then await the same future.
                notified.as_mut().enable();
            }
            // Never hold the mutex across Notify wait.
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(KeyPoolError::NoHealthyKey(service.to_string()));
            }
            tokio::select! {
                _ = notified.as_mut() => {}
                _ = tokio::time::sleep(left) => {
                    return Err(KeyPoolError::NoHealthyKey(service.to_string()));
                }
            }
        }
    }

    /// Sequential shared acquires (product still calls this until Task 5).
    /// Prefer `acquire` for new call sites — batch pin is waste under shared-cap.
    pub async fn acquire_batch(
        &self,
        service: &str,
        n: usize,
    ) -> Result<Vec<LeasedKey>, KeyPoolError> {
        let n = n.clamp(1, MAX_BATCH);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            match self.acquire(service).await {
                Ok(lease) => out.push(lease),
                Err(KeyPoolError::NoHealthyKey(_)) if !out.is_empty() => break,
                Err(e) => {
                    for k in &out {
                        let _ = self.release(k.id).await;
                    }
                    return Err(e);
                }
            }
        }
        if out.is_empty() {
            return Err(KeyPoolError::NoHealthyKey(service.to_string()));
        }
        Ok(out)
    }

    /// Release one hold without bumping `consecutive_fails` (tunnel / cancel paths).
    pub async fn release(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.release_api_key_inflight(id).await?;
        self.notify.notify_waiters();
        Ok(())
    }

    pub async fn report_success(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_success(id).await?;
        self.notify.notify_waiters();
        Ok(())
    }

    pub async fn report_failure(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_failure(id).await?;
        self.notify.notify_waiters();
        Ok(())
    }

    pub async fn report_exhausted(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_exhausted(id).await?;
        self.notify.notify_waiters();
        Ok(())
    }
}

fn to_lease(row: ApiKeyRow) -> LeasedKey {
    LeasedKey {
        id: row.id,
        service: row.service,
        key: row.key,
    }
}

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
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
        assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
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

    #[tokio::test]
    async fn acquire_batch_sequential_shared() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        db.insert_api_key("tavily", "tvly-a").await.unwrap();
        db.insert_api_key("tavily", "tvly-b").await.unwrap();
        let pool = pool_with(db, 3, Duration::from_secs(5));
        let batch = pool.acquire_batch("tavily", 2).await.unwrap();
        assert_eq!(batch.len(), 2);
        for k in &batch {
            pool.release(k.id).await.unwrap();
        }
    }

    #[tokio::test]
    async fn acquire_batch_empty_is_no_healthy() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let pool = pool_with(db, 3, Duration::from_secs(5));
        let err = pool.acquire_batch("tavily", 3).await.unwrap_err();
        assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
    }
}
