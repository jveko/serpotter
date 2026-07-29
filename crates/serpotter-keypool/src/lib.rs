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

const DEFAULT_MAX_INFLIGHT: i64 = 3;
const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Error)]
pub enum KeyPoolError {
    #[error(transparent)]
    Db(#[from] DbError),
    /// No active keys for the service (fail-fast; does not wait).
    #[error("no healthy key for service {0}")]
    NoHealthyKey(String),
    /// Active keys exist but all were at `max_inflight` until acquire deadline.
    #[error("all {0} keys busy (acquire timeout)")]
    AcquireTimeout(String),
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
    /// At-cap through deadline → `AcquireTimeout` (distinct from empty inventory).
    ///
    /// `Notified` is pinned and `enable()`d **before** taking the mutex. Report/release call
    /// `notify_waiters` without the lock, so enable-under-lock still loses wakes between
    /// "acquire failed" and `enable()`. Pre-lock enable + recheck under lock covers:
    /// - free+notify before enable → recheck sees free capacity
    /// - free+notify after enable → future is ready when we await
    pub async fn acquire(&self, service: &str) -> Result<LeasedKey, KeyPoolError> {
        let deadline = Instant::now() + self.acquire_timeout;
        loop {
            let mut notified = pin!(self.notify.notified());
            // Register before lock: reporters do not hold this mutex.
            notified.as_mut().enable();
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
            }
            // Never hold the mutex across Notify wait.
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                // Final recheck: capacity may have freed during the last wait slice.
                return self.try_acquire_once(service).await;
            }
            tokio::select! {
                _ = notified.as_mut() => {}
                _ = tokio::time::sleep(left) => {
                    // Final recheck after timeout (notify may have raced with sleep).
                    return self.try_acquire_once(service).await;
                }
            }
        }
    }

    /// One critical-section attempt (no wait). Used after deadline and for tests.
    /// Empty inventory → `NoHealthyKey`; inventory still at cap → `AcquireTimeout`.
    async fn try_acquire_once(&self, service: &str) -> Result<LeasedKey, KeyPoolError> {
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
        Err(KeyPoolError::AcquireTimeout(service.to_string()))
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

    /// Permanent ban / revoke: hard-DELETE the key row and wake waiters.
    /// Missing id is success (idempotent for multi-hold / double finish).
    /// Does not bump consecutive_fails — the row is gone.
    pub async fn report_banned(&self, id: i64) -> Result<(), KeyPoolError> {
        let _deleted = self.db.delete_api_key(id).await?;
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
mod tests;
