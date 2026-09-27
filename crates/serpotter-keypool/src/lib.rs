//! Lean in-process key pool over sqlx `api_keys`.
//!
//! Shared soft cap (`max_inflight` per key) with wait/notify when inventory exists
//! but all keys are at cap. Durable holds live in SQLite (`inflight` + `lease_until`).
//! **Single-process only** — mutex + Notify are not multi-instance safe.

use std::pin::pin;
use std::time::{Duration, Instant};

use serpotter_db::{Db, DbError, KeyLease, KeyPostState};
use thiserror::Error;
use tokio::sync::{Mutex, Notify};

const DEFAULT_MAX_INFLIGHT: i64 = 3;
const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 30;
const DEFAULT_UNKNOWN_CREDIT_WEIGHT: i64 = serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT;

/// Upper bounds for the `KEY_*` tunables. A value outside the documented range
/// warns and falls back to the compiled default (never a silent clamp), the
/// same discipline `REQUEST_TIMEOUT_SECS` already applies with its 24 h ceiling.
const MAX_HOLD_TTL_SECS: i64 = 86_400;
const MAX_ACQUIRE_TIMEOUT_SECS: u64 = 3_600;
const MAX_INFLIGHT: i64 = 1_000;
const MAX_UNKNOWN_CREDIT_WEIGHT: i64 = 1_000_000;

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
    pub token: i64,
    pub service: String,
    pub key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyLeaseRef {
    pub id: i64,
    pub token: i64,
}

impl LeasedKey {
    pub fn identity(&self) -> KeyLeaseRef {
        KeyLeaseRef {
            id: self.id,
            token: self.token,
        }
    }
}

/// The state change ONE health report caused on a key row, derived from the
/// pre/post pair the db layer returns — never a `bool`, because "the report
/// landed" says nothing about WHAT it did to the row. `None` = the report
/// changed no observable state (a failure below the disable threshold, an
/// exhausted report on already-zero credits) or the lease was already gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyTransition {
    None,
    /// `active` flipped 1 → 0: three consecutive auth failures took the key
    /// out of rotation.
    Disabled,
    /// Tracked credits changed value and are now 0. NOT "credits are 0" —
    /// zero-credit rows stay acquirable (demoted, not filtered), so a repeat
    /// report on an already-zero key would otherwise re-count forever.
    CreditsZeroed,
    /// Vendor-side account suspension (`active = 0`,
    /// `disabled_reason = 'vendor_suspended'`).
    Suspended,
    /// The row was hard-DELETED (proven vendor ban).
    Deleted,
}

/// Map a db post-state onto the transition it caused. `service` only names the
/// vendor in the log line; the mapping never depends on it.
fn transition_for(post: &KeyPostState, service: &str, key_id: i64) -> KeyTransition {
    if !post.existed {
        return KeyTransition::None;
    }
    // The disable counts ONLY when the value actually flipped in THIS report.
    // The post state alone cannot prove that: leases OVERLAP (`max_inflight`
    // defaults to 3), so legs B and C can still be holding this key when leg A
    // trips the fail threshold, and they then finish against an already-inactive
    // row. Gating on `active_before` is what keeps one key leaving rotation
    // from being counted once per in-flight leg.
    if post.active_before && !post.active {
        tracing::warn!(
            key_id,
            service,
            consecutive_fails = post.consecutive_fails,
            "api key disabled after consecutive failures"
        );
        return KeyTransition::Disabled;
    }
    if post.credits_remaining == Some(0) && post.credits_before != Some(0) {
        tracing::info!(
            key_id,
            service,
            "api key credits exhausted; demoted to the back of the rotation"
        );
        return KeyTransition::CreditsZeroed;
    }
    KeyTransition::None
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
    unknown_credit_weight: i64,
}

impl KeyPool {
    /// Build from env: `KEY_MAX_INFLIGHT` (3), `KEY_ACQUIRE_TIMEOUT_SECS` (30),
    /// `KEY_HOLD_TTL_SECS` (90 / `serpotter_db::KEY_HOLD_TTL_SECS`),
    /// `KEY_UNKNOWN_CREDIT_WEIGHT` (100 / `serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT`).
    ///
    /// Every `KEY_*` tunable follows one discipline: an unparseable **or
    /// out-of-range** value warns loudly and falls back to the compiled default.
    /// The env path never silently clamps. Accepted ranges are `1..=86_400`
    /// (`KEY_HOLD_TTL_SECS`), `1..=3_600` (`KEY_ACQUIRE_TIMEOUT_SECS`),
    /// `1..=1_000` (`KEY_MAX_INFLIGHT`) and `1..=1_000_000`
    /// (`KEY_UNKNOWN_CREDIT_WEIGHT`). In particular
    /// `KEY_ACQUIRE_TIMEOUT_SECS=0` warns and becomes the 30 s default instead of
    /// silently turning the pool into a fail-immediately pool.
    ///
    /// Additionally, `KEY_HOLD_TTL_SECS` below the acquire timeout or the
    /// effective `REQUEST_TIMEOUT_SECS` triggers a misconfiguration warning:
    /// hold-reclaim then makes `AcquireTimeout` the normal wait outcome instead
    /// of a tuned timeout.
    pub fn new(db: Db) -> Self {
        let hold_ttl_secs = env_i64_ranged(
            "KEY_HOLD_TTL_SECS",
            serpotter_db::KEY_HOLD_TTL_SECS,
            1,
            MAX_HOLD_TTL_SECS,
        );
        let acquire_timeout_secs = env_u64_ranged(
            "KEY_ACQUIRE_TIMEOUT_SECS",
            DEFAULT_ACQUIRE_TIMEOUT_SECS,
            1,
            MAX_ACQUIRE_TIMEOUT_SECS,
        );
        warn_if_hold_below_timeout(hold_ttl_secs, Duration::from_secs(acquire_timeout_secs));
        let request_timeout = std::env::var("REQUEST_TIMEOUT_SECS").ok();
        warn_if_hold_below_request_timeout(hold_ttl_secs, request_timeout.as_deref());
        Self::with_config(
            db,
            env_i64_ranged("KEY_MAX_INFLIGHT", DEFAULT_MAX_INFLIGHT, 1, MAX_INFLIGHT),
            Duration::from_secs(acquire_timeout_secs),
            hold_ttl_secs,
            env_i64_ranged(
                "KEY_UNKNOWN_CREDIT_WEIGHT",
                DEFAULT_UNKNOWN_CREDIT_WEIGHT,
                1,
                MAX_UNKNOWN_CREDIT_WEIGHT,
            ),
        )
    }

    /// Explicit limits (tests and callers that cannot rely on process env).
    pub fn with_config(
        db: Db,
        max_inflight: i64,
        acquire_timeout: Duration,
        hold_ttl_secs: i64,
        unknown_credit_weight: i64,
    ) -> Self {
        Self {
            db,
            lock: Mutex::new(()),
            notify: Notify::new(),
            max_inflight: max_inflight.max(1),
            acquire_timeout,
            hold_ttl_secs: hold_ttl_secs.max(1),
            unknown_credit_weight: unknown_credit_weight.max(1),
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

    pub fn unknown_credit_weight(&self) -> i64 {
        self.unknown_credit_weight
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
                    .acquire_api_key_shared(
                        service,
                        self.max_inflight,
                        self.hold_ttl_secs,
                        self.unknown_credit_weight,
                    )
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
            .acquire_api_key_shared(
                service,
                self.max_inflight,
                self.hold_ttl_secs,
                self.unknown_credit_weight,
            )
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
    pub async fn release(&self, lease: KeyLeaseRef) -> Result<(), KeyPoolError> {
        if !self.db.release_api_key_lease(lease.token).await? {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key lease release found no live holder; lease was already reclaimed"
            );
        }
        self.notify.notify_waiters();
        Ok(())
    }

    /// Re-stamp this holder's child lease. Returns false when the holder was
    /// already reclaimed; callers should treat their lease as lost.
    pub async fn refresh_hold(&self, lease: KeyLeaseRef) -> Result<bool, KeyPoolError> {
        let refreshed = self
            .db
            .refresh_api_key_lease(lease.token, self.hold_ttl_secs)
            .await?;
        if !refreshed {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key lease refresh found no live holder; lease lost"
            );
        }
        Ok(refreshed)
    }

    pub async fn report_success(&self, lease: KeyLeaseRef) -> Result<(), KeyPoolError> {
        if !self.db.report_api_key_success_lease(lease.token).await? {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key success report found no live holder; lease was already reclaimed"
            );
        }
        self.notify.notify_waiters();
        Ok(())
    }

    /// Report an auth-class failure. Returns the transition it caused: the
    /// third consecutive failure reports [`KeyTransition::Disabled`], earlier
    /// ones [`KeyTransition::None`].
    pub async fn report_failure(
        &self,
        lease: KeyLeaseRef,
        service: &str,
    ) -> Result<KeyTransition, KeyPoolError> {
        let post = self.db.report_api_key_failure_lease(lease.token).await?;
        if !post.existed {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key failure report found no live holder; lease was already reclaimed"
            );
        }
        self.notify.notify_waiters();
        Ok(transition_for(&post, service, lease.id))
    }

    /// Report a 429/upstream "no credits left". Credits `NULL` stay `NULL`, so
    /// an untracked key can never report a zeroing. `cooldown_secs` is stamped
    /// into `api_keys.cooldown_until` in the same statement; the acquire path
    /// reads it as a demotion tier, so a cooling key still serves when it is
    /// the only key left.
    pub async fn report_exhausted(
        &self,
        lease: KeyLeaseRef,
        service: &str,
        cooldown_secs: i64,
    ) -> Result<KeyTransition, KeyPoolError> {
        let post = self
            .db
            .report_api_key_exhausted_lease(lease.token, cooldown_secs)
            .await?;
        if !post.existed {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key exhausted report found no live holder; lease was already reclaimed"
            );
        }
        self.notify.notify_waiters();
        Ok(transition_for(&post, service, lease.id))
    }

    /// Upstream `402` (payment required): demote the key by zeroing tracked
    /// credits even when they are `NULL`. See [`KeyPool::report_exhausted`].
    pub async fn report_payment_required(
        &self,
        lease: KeyLeaseRef,
        service: &str,
    ) -> Result<KeyTransition, KeyPoolError> {
        let post = self
            .db
            .report_api_key_payment_required_lease(lease.token)
            .await?;
        if !post.existed {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key payment-required report found no live holder; lease was already reclaimed"
            );
        }
        self.notify.notify_waiters();
        Ok(transition_for(&post, service, lease.id))
    }

    /// Permanent ban / revoke: hard-DELETE the key row and wake waiters.
    ///
    /// The db `bool` is "a row was actually deleted": a no-op (multi-hold /
    /// double finish) is deliberately NOT reported as a transition.
    pub async fn revoke_key_row(&self, id: i64) -> Result<KeyTransition, KeyPoolError> {
        let deleted = self.db.delete_api_key(id).await?;
        self.notify.notify_waiters();
        if deleted {
            tracing::warn!(key_id = id, "api key row deleted after a proven vendor ban");
            return Ok(KeyTransition::Deleted);
        }
        Ok(KeyTransition::None)
    }

    /// Likely vendor ban (soft tier, non-firecrawl): disable the row and stamp
    /// `disabled_reason = 'vendor_suspended'`, which takes it permanently out
    /// of rotation — the 24h re-enable cron skips marked rows.
    pub async fn report_suspended(
        &self,
        lease: KeyLeaseRef,
        service: &str,
    ) -> Result<KeyTransition, KeyPoolError> {
        let post = self.db.suspend_api_key_lease(lease.token).await?;
        self.notify.notify_waiters();
        if !post.existed {
            tracing::warn!(
                key_id = lease.id,
                lease_token = lease.token,
                "key suspension report found no live holder; lease was already reclaimed"
            );
            return Ok(KeyTransition::None);
        }
        // Same PRE gate as the Disabled arm, for the same reason: the
        // suspension SQL is unconditional, so every OTHER leg still holding a
        // lease on this row would otherwise report `Suspended` again. A row
        // that was leased at all was active when it was leased, so the gate
        // loses no legitimate transition.
        if !post.active_before {
            return Ok(KeyTransition::None);
        }
        tracing::warn!(
            key_id = lease.id,
            service,
            "api key suspended by the vendor (account deactivated)"
        );
        Ok(KeyTransition::Suspended)
    }
}

fn to_lease(row: KeyLease) -> LeasedKey {
    LeasedKey {
        id: row.key.id,
        token: row.token,
        service: row.key.service,
        key: row.key.key,
    }
}

/// Read a `KEY_*` var, warning (never silently defaulting) when it is set but
/// not valid UTF-8. `VarError::NotPresent` is the only silent case — `.ok()`
/// would swallow `NotUnicode` and make a misconfigured deployment look unset.
fn env_var(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            tracing::warn!(
                var = key,
                "KEY_* env value is not valid UTF-8; using compiled default"
            );
            None
        }
    }
}

/// `Some(parsed)` for a set, parseable value; `None` for missing or unparseable
/// (the latter warns loudly, never silently).
fn parse_env_i64_opt(
    key: &str,
    raw: Option<String>,
    default: i64,
    min: i64,
    max: i64,
) -> Option<i64> {
    match raw {
        Some(value) => match value.parse::<i64>() {
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    var = key,
                    raw_value = %value,
                    min,
                    max,
                    using = default,
                    "KEY_* env value is not a valid integer; using compiled default"
                );
                None
            }
        },
        None => None,
    }
}

/// `KEY_*` read with the shared range discipline: an unparseable **or**
/// out-of-range value warns and falls back to `default`; a value inside
/// `min..=max` is used as-is.
fn env_i64_ranged(key: &str, default: i64, min: i64, max: i64) -> i64 {
    ranged(
        key,
        parse_env_i64_opt(key, env_var(key), default, min, max),
        default,
        min,
        max,
    )
}

fn parse_env_u64_opt(
    key: &str,
    raw: Option<String>,
    default: u64,
    min: u64,
    max: u64,
) -> Option<u64> {
    match raw {
        Some(value) => match value.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => {
                tracing::warn!(
                    var = key,
                    raw_value = %value,
                    min,
                    max,
                    using = default,
                    "KEY_* env value is not a valid unsigned integer; using compiled default"
                );
                None
            }
        },
        None => None,
    }
}

/// `KEY_*` read with the shared range discipline, unsigned variant: an
/// unparseable **or** out-of-range value warns and falls back to `default`.
fn env_u64_ranged(key: &str, default: u64, min: u64, max: u64) -> u64 {
    ranged(
        key,
        parse_env_u64_opt(key, env_var(key), default, min, max).map(as_i64),
        default as i64,
        min as i64,
        max as i64,
    ) as u64
}

/// Shared range gate for both `KEY_*` reads: use the parsed value only when it
/// lands inside `min..=max`, otherwise warn (naming the var, the offending
/// value and the accepted range) and fall back to the compiled default.
fn ranged(key: &str, parsed: Option<i64>, default: i64, min: i64, max: i64) -> i64 {
    match parsed {
        Some(n) if n >= min && n <= max => n,
        Some(n) => {
            tracing::warn!(
                var = key,
                value = n,
                min,
                max,
                using = default,
                "KEY_* env value out of range; using compiled default"
            );
            default
        }
        None => default,
    }
}

/// `u64` → `i64` for the range gate. Values that cannot fit are folded to
/// `i64::MAX`, which is above every documented ceiling, so they are reported
/// as out of range (the alternative, `as` truncation, would be a silent
/// misparse).
fn as_i64(n: u64) -> i64 {
    n.min(i64::MAX as u64) as i64
}

/// Warn when holds expire before the effective request deadline. The API
/// normalizes the same raw value; see `request_timeout_from_env()` there.
fn warn_if_hold_below_request_timeout(hold_ttl_secs: i64, raw: Option<&str>) {
    let request_timeout = effective_request_timeout_secs(raw);
    if hold_ttl_secs > 0 && (hold_ttl_secs as u64) < request_timeout {
        tracing::warn!(
            hold_ttl_secs,
            request_timeout_secs = request_timeout,
            "KEY_HOLD_TTL_SECS < REQUEST_TIMEOUT_SECS: holds may expire before requests complete"
        );
    }
}

/// Mirrors the API's `request_timeout_from_env()` normalization because the
/// pool cannot depend on the API crate. Unset, empty, zero, invalid, and values
/// over 24 hours use the same 120-second effective default.
fn effective_request_timeout_secs(raw: Option<&str>) -> u64 {
    const DEFAULT: u64 = 120;
    const MAX: u64 = 86_400;
    raw.and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0 && *secs <= MAX)
        .unwrap_or(DEFAULT)
}

fn warn_if_hold_below_timeout(hold_ttl_secs: i64, acquire_timeout: Duration) {
    if hold_ttl_secs > 0 && (hold_ttl_secs as u64) < acquire_timeout.as_secs() {
        tracing::warn!(
            hold_ttl_secs,
            acquire_timeout_secs = acquire_timeout.as_secs(),
            "KEY_HOLD_TTL_SECS < KEY_ACQUIRE_TIMEOUT_SECS: hold-reclaim makes acquire-timeout the normal wait path"
        );
    }
}

#[cfg(test)]
mod tests;
