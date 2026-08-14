//! RAII hold guards for key and proxy leases.
//!
//! Explicit `finish_*` + `disarm` on every return path. Drop only
//! `tokio::spawn`s best-effort `release` (never `block_on`); hold TTL is the
//! safety net if the spawn is lost with the runtime.
//!
//! **Disarm only on Ok:** if report/release returns Err, leave the guard armed
//! so Drop still attempts `release` and inflight is not stranded solely by a
//! failed explicit finish.

use std::sync::Arc;

use serpotter_keypool::KeyPool;
use serpotter_outbound::{ProxyLease, ProxyPool};

/// Cap stored node last_error so admin UI / DB stay readable.
pub(crate) fn truncate_err(msg: &str) -> String {
    const MAX: usize = 240;
    if msg.chars().count() <= MAX {
        return msg.to_string();
    }
    let mut out: String = msg.chars().take(MAX).collect();
    out.push('…');
    out
}

/// Owned, clonable refresh handle for a held key — handed to long-running
/// ladder closures (poll loops) so they can re-stamp `lease_until` mid-hold
/// without borrowing the ladder's guard. Best-effort: a failed refresh never
/// aborts the caller.
#[derive(Clone)]
pub struct KeyRefresh {
    keys: Arc<KeyPool>,
    id: i64,
}

impl KeyRefresh {
    pub fn new(keys: Arc<KeyPool>, id: i64) -> Self {
        Self { keys, id }
    }

    pub async fn refresh(&self) {
        let _ = self.keys.refresh_hold(self.id).await;
    }
}

/// Owned, clonable refresh handle for a held node (same contract as
/// [`KeyRefresh`]).
#[derive(Clone)]
pub struct ProxyRefresh {
    outbound: Arc<ProxyPool>,
    lease: ProxyLease,
}

impl ProxyRefresh {
    pub fn new(outbound: Arc<ProxyPool>, lease: ProxyLease) -> Self {
        Self { outbound, lease }
    }

    pub async fn refresh(&self) {
        let _ = self.outbound.refresh(&self.lease).await;
    }
}

/// Key-side hold: explicit finish_* + disarm; Drop → spawn release only.
pub struct KeyHold {
    keys: Arc<KeyPool>,
    id: i64,
    disarmed: bool,
}

impl KeyHold {
    pub fn new(keys: Arc<KeyPool>, id: i64) -> Self {
        Self {
            keys,
            id,
            disarmed: false,
        }
    }

    pub async fn finish_success(&mut self) {
        if self.keys.report_success(self.id).await.is_ok() {
            self.disarm();
        }
    }

    pub async fn finish_failure(&mut self) {
        if self.keys.report_failure(self.id).await.is_ok() {
            self.disarm();
        }
    }

    pub async fn finish_exhausted(&mut self) {
        if self.keys.report_exhausted(self.id).await.is_ok() {
            self.disarm();
        }
    }

    /// Key row id for tracing (never log the secret key material).
    pub fn key_id(&self) -> i64 {
        self.id
    }

    /// Permanent provider ban: hard-delete key row (no consecutive_fails++).
    pub async fn finish_banned(&mut self) {
        if self.keys.report_banned(self.id).await.is_ok() {
            self.disarm();
        }
    }

    /// Tunnel / cancel path: inflight-- only, no consecutive_fails++.
    pub async fn finish_release(&mut self) {
        if self.keys.release(self.id).await.is_ok() {
            self.disarm();
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for KeyHold {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let keys = Arc::clone(&self.keys);
        let id = self.id;
        // Never block_on in Drop — spawn best-effort release only.
        tokio::spawn(async move {
            let _ = keys.release(id).await;
        });
    }
}

/// Proxy-side hold: same finish/disarm discipline as [`KeyHold`].
pub struct ProxyHold {
    outbound: Arc<ProxyPool>,
    lease: ProxyLease,
    disarmed: bool,
}

impl ProxyHold {
    pub fn new(outbound: Arc<ProxyPool>, lease: ProxyLease) -> Self {
        Self {
            outbound,
            lease,
            disarmed: false,
        }
    }

    pub async fn finish_success(&mut self) {
        if self.outbound.report_success(&self.lease).await.is_ok() {
            self.disarm();
        }
    }

    pub async fn finish_failure(&mut self, error: Option<&str>) {
        if self
            .outbound
            .report_failure(&self.lease, error)
            .await
            .is_ok()
        {
            self.disarm();
        }
    }

    /// Inflight-- without blaming node health (key fault / non-tunnel paths).
    pub async fn finish_release(&mut self) {
        if self.outbound.release(&self.lease).await.is_ok() {
            self.disarm();
        }
    }

    /// Node row id for tracing / ExecMeta (never log proxy credentials).
    pub fn node_id(&self) -> i64 {
        self.lease.node_id
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for ProxyHold {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let outbound = Arc::clone(&self.outbound);
        let lease = self.lease.clone();
        tokio::spawn(async move {
            let _ = outbound.release(&lease).await;
        });
    }
}
