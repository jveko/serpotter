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

use serpotter_keypool::{KeyLeaseRef, KeyPool};
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

/// Strip URL userinfo (`scheme://user:pass@host`) down to `scheme://host` for
/// anything persisted to `nodes.last_error`.
///
/// Defense in depth, NOT a fix for an observed leak. Measured against reqwest
/// 0.12 (2026-09-26): a bad proxy URL yields a bare `"builder error"` and a
/// proxied connect failure yields `"error sending request for url
/// (http://target/)"` — neither echoes the proxy URL, so no credential is
/// reaching `last_error` today. The guard is here because that is a property
/// of a dependency's error formatting, not a contract it publishes: a reqwest
/// upgrade that starts quoting the failing URL would otherwise publish the
/// node's percent-encoded `username:password` to every admin reader
/// (`admin/nodes.rs` serializes `last_error` verbatim). `serpotter_outbound::
/// test_node` redacts at its call site for the same reason. The parse reason
/// (the part an operator needs) is kept intact.
pub(crate) fn redact_url_userinfo(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(scheme_end) = rest.find("://") {
        let authority_start = scheme_end + 3;
        let after = &rest[authority_start..];
        // Authority ends at the first delimiter reqwest would treat as one.
        let authority_len = after
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ')' | ']'))
            .unwrap_or(after.len());
        let authority = &after[..authority_len];
        out.push_str(&rest[..authority_start]);
        match authority.rsplit_once('@') {
            Some((_userinfo, host)) => out.push_str(host),
            None => out.push_str(authority),
        }
        rest = &after[authority_len..];
    }
    out.push_str(rest);
    out
}

/// Redact then cap — the order every `nodes.last_error` write must use.
pub(crate) fn safe_node_error(msg: &str) -> String {
    truncate_err(&redact_url_userinfo(msg))
}

/// Owned, clonable refresh handle for a held key — handed to long-running
/// ladder closures (poll loops) so they can re-stamp `lease_until` mid-hold
/// without borrowing the ladder's guard. Best-effort: a failed refresh never
/// aborts the caller, but `refresh` reports the outcome — `false` = the hold
/// was LOST (reclaimed after TTL or released underneath), logged, never
/// silent.
#[derive(Clone)]
pub struct KeyRefresh {
    keys: Arc<KeyPool>,
    lease: KeyLeaseRef,
}

impl KeyRefresh {
    pub fn new(keys: Arc<KeyPool>, lease: KeyLeaseRef) -> Self {
        Self { keys, lease }
    }

    pub async fn refresh(&self) -> bool {
        match self.keys.refresh_hold(self.lease).await {
            Ok(refreshed) => refreshed,
            Err(e) => {
                tracing::warn!(key_id = self.lease.id, lease_token = self.lease.token, error = %e, "key lease refresh failed");
                false
            }
        }
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

    pub async fn refresh(&self) -> bool {
        match self.outbound.refresh(&self.lease).await {
            Ok(refreshed) => refreshed,
            Err(e) => {
                tracing::warn!(node_id = self.lease.node_id, lease_token = self.lease.token, error = %e, "node lease refresh failed");
                false
            }
        }
    }
}

/// Key-side hold: explicit finish_* + disarm; Drop → spawn release only.
pub struct KeyHold {
    keys: Arc<KeyPool>,
    lease: KeyLeaseRef,
    disarmed: bool,
}

impl KeyHold {
    pub fn new(keys: Arc<KeyPool>, lease: KeyLeaseRef) -> Self {
        Self {
            keys,
            lease,
            disarmed: false,
        }
    }

    pub async fn finish_success(&mut self) {
        if self.keys.report_success(self.lease).await.is_ok() {
            self.disarm();
        }
    }
    pub async fn finish_failure(&mut self) {
        if self.keys.report_failure(self.lease).await.is_ok() {
            self.disarm();
        }
    }
    pub async fn finish_exhausted(&mut self) {
        if self.keys.report_exhausted(self.lease).await.is_ok() {
            self.disarm();
        }
    }
    pub async fn finish_payment_required(&mut self) {
        if self.keys.report_payment_required(self.lease).await.is_ok() {
            self.disarm();
        }
    }
    pub fn key_id(&self) -> i64 {
        self.lease.id
    }
    pub async fn finish_banned(&mut self) {
        if self.keys.revoke_key_row(self.lease.id).await.is_ok() {
            self.disarm();
        }
    }
    /// Vendor-deactivation disable: `active=0` +
    /// `disabled_reason = 'vendor_suspended'`. PERMANENT since schema 18 —
    /// the `KEY_REENABLE_AFTER_HOURS` cron deliberately skips that reason, so
    /// an operator re-enable is the only way back. Callers must reach this
    /// only on a proven account-state phrase (see `search::banned`).
    pub async fn finish_suspended(&mut self) {
        if self.keys.report_suspended(self.lease).await.is_ok() {
            self.disarm();
        }
    }
    pub async fn finish_release(&mut self) {
        if self.keys.release(self.lease).await.is_ok() {
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
        let lease = self.lease;
        tokio::spawn(async move {
            let _ = keys.release(lease).await;
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

#[cfg(test)]
mod tests {
    use super::{safe_node_error, KeyHold};
    use serpotter_db::connect_and_migrate;
    use serpotter_keypool::KeyPool;
    use std::sync::Arc;
    use std::time::Duration;

    fn pool(db: serpotter_db::Db) -> KeyPool {
        KeyPool::with_config(
            db,
            1,
            Duration::from_secs(5),
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
    }

    async fn inflight(db: &serpotter_db::Db, key_id: i64) -> i64 {
        db.get_api_key_admin(key_id)
            .await
            .unwrap()
            .unwrap()
            .inflight
    }

    #[tokio::test]
    async fn armed_key_hold_drop_releases_its_inflight_slot() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        db.insert_api_key("tavily", "tvly-armed-drop")
            .await
            .unwrap();
        let pool = Arc::new(pool(db.clone()));
        let lease = pool.acquire("tavily").await.unwrap();
        assert_eq!(inflight(&db, lease.id).await, 1);

        drop(KeyHold::new(Arc::clone(&pool), lease.identity()));
        let replacement = tokio::time::timeout(Duration::from_secs(2), pool.acquire("tavily"))
            .await
            .expect("armed Drop must release the held slot")
            .expect("pool must accept the replacement after Drop releases");

        assert_eq!(inflight(&db, replacement.id).await, 1);
        pool.release(replacement.identity()).await.unwrap();
    }

    #[tokio::test]
    async fn disarmed_key_hold_drop_does_not_release_a_reacquired_slot() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let pool = Arc::new(pool(db.clone()));
        db.insert_api_key("tavily", "tvly-disarmed-drop")
            .await
            .unwrap();
        let first = pool.acquire("tavily").await.unwrap();
        let mut hold = KeyHold::new(Arc::clone(&pool), first.identity());

        hold.finish_release().await;
        let second = pool.acquire("tavily").await.unwrap();
        assert_ne!(first.token, second.token);

        drop(hold);
        // Drop's release is spawned best-effort; give that task a chance to
        // corrupt the replacement before checking that it did not.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            inflight(&db, second.id).await,
            1,
            "a disarmed guard must not release the slot now held by its replacement"
        );
        pool.release(second.identity()).await.unwrap();
    }

    /// A client-build error embeds the proxy URL reqwest refused to parse, and
    /// that URL carries the node's percent-encoded `username:password`. It
    /// lands in `nodes.last_error`, which the admin API serializes verbatim,
    /// so credentials must be gone while the part an operator needs (scheme,
    /// host, port, parse reason) survives. Same exposure
    /// `serpotter_outbound::test_node` redacts at its call site.
    #[test]
    fn node_error_redacts_proxy_credentials() {
        let raw = "builder error: builder error for url \
                   (http://S3CR3T%40marker:hunter2@proxy.example:8080/): \
                   relative URL without a base";
        let safe = safe_node_error(raw);
        assert!(
            !safe.contains("hunter2"),
            "the node password must never be persisted: {safe}"
        );
        assert!(
            !safe.contains("S3CR3T"),
            "the node username must never be persisted: {safe}"
        );
        assert!(
            !safe.contains('%'),
            "percent-encoded userinfo must be gone entirely: {safe}"
        );
        assert!(
            safe.contains("proxy.example:8080"),
            "the operator still needs the endpoint: {safe}"
        );
        assert!(
            safe.contains("relative URL without a base"),
            "the parse reason must survive: {safe}"
        );
    }

    /// Redaction must leave ordinary text alone, and the cap must still apply
    /// — the two properties `safe_node_error` composes.
    #[test]
    fn node_error_keeps_plain_text_and_caps_length() {
        assert_eq!(
            safe_node_error("connection refused by proxy.example:8080"),
            "connection refused by proxy.example:8080"
        );
        let long = "x".repeat(400);
        assert_eq!(safe_node_error(&long).chars().count(), 241);
    }
}
