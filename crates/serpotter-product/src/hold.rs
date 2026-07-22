//! RAII hold guards for key and proxy leases.
//!
//! Explicit `finish_*` + `disarm` on every return path. Drop only
//! `tokio::spawn`s best-effort `release` (never `block_on`); hold TTL is the
//! safety net if the spawn is lost with the runtime.

use std::sync::Arc;

use serpotter_keypool::KeyPool;
use serpotter_outbound::{ProxyLease, ProxyPool};

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
        let _ = self.keys.report_success(self.id).await;
        self.disarm();
    }

    pub async fn finish_failure(&mut self) {
        let _ = self.keys.report_failure(self.id).await;
        self.disarm();
    }

    pub async fn finish_exhausted(&mut self) {
        let _ = self.keys.report_exhausted(self.id).await;
        self.disarm();
    }

    /// Tunnel / cancel path: inflight-- only, no consecutive_fails++.
    pub async fn finish_release(&mut self) {
        let _ = self.keys.release(self.id).await;
        self.disarm();
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
        let _ = self.outbound.report_success(&self.lease).await;
        self.disarm();
    }

    pub async fn finish_failure(&mut self) {
        let _ = self.outbound.report_failure(&self.lease).await;
        self.disarm();
    }

    /// Inflight-- without blaming node health (key fault / non-tunnel paths).
    pub async fn finish_release(&mut self) {
        let _ = self.outbound.release(&self.lease).await;
        self.disarm();
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
