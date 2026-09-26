//! Admission control + legacy-session ownership for the `/mcp` transport.
//!
//! Two independent maps, both process-local and both bounded:
//!
//! - **Per-token in-flight cap.** The spec lists "rate limit tool
//!   invocations" under a server's security considerations; without a
//!   per-client ceiling one token can enqueue unbounded research jobs (each
//!   fanning out to search + N scrapes + xAI synthesis against paid vendor
//!   keys). The cap is a `try_acquire` on a per-token semaphore, so a full
//!   bucket is REFUSED immediately (retryable envelope) instead of queued —
//!   and the refusal happens in the tool handler BEFORE the per-request
//!   progress delivery task is spawned, so an over-cap caller cannot multiply
//!   tasks either.
//! - **Session ↔ token binding.** rmcp's `LocalSessionManager` mints opaque
//!   UUIDs and answers POST/GET/DELETE for any id it still holds; it has no
//!   notion of who created one. Without a binding, a leaked
//!   `Mcp-Session-Id` lets any valid token drive (GET SSE) or terminate
//!   (DELETE) another tenant's session. Binding the id to the
//!   [`serpotter_db::TokenRow`] that opened it, and answering a mismatch with
//!   the same 404 as an unknown id, restores ownership without leaking
//!   whether the id exists at all.
//!
//! Both maps share the legacy keep-alive window ([`MCP_SESSION_TTL_SECS`]):
//! entries older than the TTL are dead by construction (rmcp has already
//! evicted the session), so they are pruned on every write/read and the maps
//! cannot grow without bound.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Concurrent long-running tool calls allowed per token. Matches the shape
/// of `KEY_MAX_INFLIGHT` on the key pool: a ceiling per client identity,
/// enforced before any vendor spend.
pub const MCP_MAX_INFLIGHT_PER_TOKEN: usize = 8;

/// Hard ceiling on remembered session bindings. Far above the number of
/// sessions a single process is expected to hold; a pruner that only runs on
/// the 1 h TTL could otherwise be starved by a client that never talks
/// again.
const MAX_SESSION_BINDINGS: usize = 4096;

/// One remembered session and the token that opened it.
#[derive(Clone, Copy)]
struct Binding {
    token_id: i64,
    seen: Instant,
}

pub(crate) struct Admission {
    max_inflight: usize,
    ttl: Duration,
    /// `token id → per-token in-flight bucket`.
    inflight: Mutex<HashMap<i64, Arc<Semaphore>>>,
    /// `session id → owning token id`.
    sessions: Mutex<HashMap<String, Binding>>,
}

impl Admission {
    pub(crate) fn new(max_inflight: usize, ttl: Duration) -> Self {
        Self {
            max_inflight: max_inflight.max(1),
            ttl,
            inflight: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Take one in-flight slot for `token_id`, or `None` when the token is
    /// already at [`MCP_MAX_INFLIGHT_PER_TOKEN`].
    ///
    /// `try_acquire` (not `acquire`) is the whole point: queueing here would
    /// re-create the unbounded growth the cap exists to prevent.
    pub(crate) fn try_acquire(&self, token_id: i64) -> Option<OwnedSemaphorePermit> {
        let sem = {
            let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            // Drop buckets no live permit references any more; a permit
            // holds a strong ref, so `Arc::strong_count == 1` means idle.
            map.retain(|_, s| Arc::strong_count(s) > 1);
            Arc::clone(
                map.entry(token_id)
                    .or_insert_with(|| Arc::new(Semaphore::new(self.max_inflight))),
            )
        };
        // A closed semaphore is a full one: both mean "no slot right now".
        sem.try_acquire_owned().ok()
    }

    /// The configured cap (used in the refusal message).
    pub(crate) fn max_inflight(&self) -> usize {
        self.max_inflight
    }

    /// Record `session_id` as owned by `token_id`. Idempotent: a repeated
    /// `initialize` on the same id (or a retry after a response loss) keeps
    /// the ORIGINAL owner, so a second holder cannot take a session over.
    pub(crate) fn bind_session(&self, session_id: &str, token_id: i64) {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        // TTL pruning is O(n) but runs on every write; the eviction scan below
        // is O(n) too, so it is gated behind the ceiling — otherwise every
        // `initialize` would pay an LRU scan to evict nothing.
        prune(&mut map, self.ttl);
        map.entry(session_id.to_string()).or_insert(Binding {
            token_id,
            seen: Instant::now(),
        });
        if map.len() <= MAX_SESSION_BINDINGS {
            return;
        }
        // Over the ceiling: evict least-recently-seen until back under it. Only
        // reachable at ~4096 remembered sessions, so the scan cost is bounded
        // and amortized against that many inserts.
        while map.len() > MAX_SESSION_BINDINGS {
            let Some(oldest) = map
                .iter()
                .min_by_key(|(_, b)| b.seen)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            map.remove(&oldest);
        }
    }

    /// The token that owns `session_id`, or `None` when the id is unknown
    /// (never bound, evicted, or already terminated). `None` is deliberately
    /// indistinguishable from "another token owns it" to the caller: both
    /// answer the same 404.
    pub(crate) fn session_token(&self, session_id: &str) -> Option<i64> {
        let mut map = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        prune(&mut map, self.ttl);
        // Refresh the timestamp in place (O(1)) rather than remove + reinsert:
        // a legacy session's binding must never lapse before the session it
        // mirrors, and rmcp's keep-alive is SLIDING (refreshed per request), so
        // an authorized request is the signal that the session is still alive.
        // The read path deliberately does no eviction scan — it is on the hot
        // path of every session-scoped request, and `bind_session` already
        // keeps the map under the ceiling.
        let binding = map.get_mut(session_id)?;
        binding.seen = Instant::now();
        Some(binding.token_id)
    }

    /// Drop a binding after the session was terminated.
    pub(crate) fn forget_session(&self, session_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session_id);
    }

    /// Current number of remembered bindings (test/diagnostic surface).
    #[cfg(test)]
    pub(crate) fn session_count(&self) -> usize {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

/// Drop entries whose TTL lapsed: rmcp's `LocalSessionManager` already
/// evicted the session, so the binding is dead weight.
fn prune(map: &mut HashMap<String, Binding>, ttl: Duration) {
    map.retain(|_, b| b.seen.elapsed() < ttl);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cap_refuses_the_ninth_concurrent_call() {
        let a = Admission::new(MCP_MAX_INFLIGHT_PER_TOKEN, Duration::from_secs(3600));
        let held: Vec<_> = (0..MCP_MAX_INFLIGHT_PER_TOKEN)
            .map(|_| a.try_acquire(7).expect("slot"))
            .collect();
        assert!(
            a.try_acquire(7).is_none(),
            "a token at the cap must be refused, not queued"
        );
        // Another token is unaffected: the cap is per identity.
        assert!(a.try_acquire(8).is_some(), "cap is per token");
        drop(held);
        assert!(a.try_acquire(7).is_some(), "released slot is reusable");
    }

    /// The eviction scan is gated behind the ceiling, so it must NOT fire on
    /// ordinary inserts — and it must fire exactly at the ceiling, dropping
    /// the least-recently-seen entries rather than growing without bound.
    #[test]
    fn eviction_only_triggers_at_the_ceiling() {
        let a = Admission::new(8, Duration::from_secs(3600));
        for i in 0..MAX_SESSION_BINDINGS {
            a.bind_session(&format!("s{i}"), 1);
        }
        assert_eq!(
            a.session_count(),
            MAX_SESSION_BINDINGS,
            "inserts below the ceiling must not evict"
        );
        // Refresh an early entry so it is not the least-recently-seen one.
        assert_eq!(a.session_token("s0"), Some(1));
        a.bind_session("overflow", 1);
        assert_eq!(
            a.session_count(),
            MAX_SESSION_BINDINGS,
            "one insert past the ceiling evicts exactly one"
        );
        assert_eq!(
            a.session_token("s0"),
            Some(1),
            "the refreshed entry survives; the genuinely oldest is evicted"
        );
        assert_eq!(a.session_token("s1"), None, "the oldest was evicted");
    }

    #[test]
    fn binding_is_sticky_and_ttl_prunes() {
        let a = Admission::new(8, Duration::from_millis(0));
        a.bind_session("s1", 1);
        // A zero TTL means the entry is already dead by the next read.
        assert_eq!(a.session_token("s1"), None);

        let a = Admission::new(8, Duration::from_secs(3600));
        a.bind_session("s1", 1);
        a.bind_session("s1", 2);
        assert_eq!(a.session_token("s1"), Some(1), "first owner wins");
        a.forget_session("s1");
        assert_eq!(a.session_token("s1"), None);
        assert_eq!(a.session_count(), 0);
    }
}
