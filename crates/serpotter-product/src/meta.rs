//! Non-wire execution metadata for request_log / spans (Approach 2 path A).

/// Accumulated per client call; never serialized on wire DTOs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecMeta {
    pub strategy: Option<String>,
    pub providers_consulted: Vec<String>,
    pub attempt_count: u32,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
    /// B1: true when the response was served from the exact-query TTL cache
    /// (zero provider calls). Read by the API layer for the request_log row and
    /// the `serpotter_cache_requests_total` metric; never a provider signal.
    pub cache_hit: bool,
    /// B2: token usage of the successful provider result(s). `None` = unknown
    /// (provider did not report usage, or no provider ran). Multi-leg requests
    /// (hybrid/blend/research) SUM the contributing successful legs.
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// B2: cost estimate (vendor `costDollars` or a credit estimate, see the
    /// provider clients). F64 because vendor costs are decimal dollars.
    pub cost: Option<f64>,
    /// Internal: sticky last-success tracking.
    had_success: bool,
}

impl ExecMeta {
    /// Record one provider attempt.
    ///
    /// - Always bumps `attempt_count` and first-seen `providers_consulted`.
    /// - On success: sets key/node (sticky last success).
    /// - On failure: sets key/node only if no success yet (last attempt).
    pub fn note_attempt(
        &mut self,
        service: &str,
        key_id: i64,
        node_id: Option<i64>,
        success: bool,
    ) {
        self.attempt_count = self.attempt_count.saturating_add(1);
        if !self.providers_consulted.iter().any(|s| s == service) {
            self.providers_consulted.push(service.to_string());
        }
        if success {
            self.key_id = Some(key_id);
            self.node_id = node_id;
            self.had_success = true;
        } else if !self.had_success {
            self.key_id = Some(key_id);
            self.node_id = node_id;
        }
    }

    /// Record an attempt that has LEASED its key/node but has not returned
    /// yet (F10 attribution). The real [`ExecMeta::note_attempt`] for the same
    /// call follows when the provider answers.
    ///
    /// Deliberately a strict SUBSET of `note_attempt`'s effects:
    /// - does NOT bump `attempt_count` (an unfinished call is not a completed
    ///   attempt);
    /// - does NOT set `had_success`, and only writes key/node while no leg has
    ///   succeeded yet — the same `!had_success` guard `note_attempt`'s failure
    ///   arm uses.
    ///
    /// The guard is load-bearing: a multi-leg request (blend-verify) that has
    /// already succeeded on one vendor and then times out mid-call on the next
    /// would otherwise have its sticky last-success key/node overwritten by the
    /// in-flight leg, misattributing the event AND the `usage_daily` rollup to
    /// the wrong key. The vendor is still recorded either way, which is what a
    /// timeout needs to name.
    pub fn note_attempt_pending(&mut self, service: &str, key_id: i64, node_id: Option<i64>) {
        if !self.providers_consulted.iter().any(|s| s == service) {
            self.providers_consulted.push(service.to_string());
        }
        if !self.had_success {
            self.key_id = Some(key_id);
            self.node_id = node_id;
        }
    }

    /// Comma-separated, no spaces, first-seen order. `None` if empty.
    pub fn providers_csv(&self) -> Option<String> {
        if self.providers_consulted.is_empty() {
            None
        } else {
            Some(self.providers_consulted.join(","))
        }
    }

    /// B1: mark this meta as a cache serve (no provider call). The API layer
    /// surfaces it via request_log / metrics; the wire `cache_hit` field is set
    /// by the product layer on the response itself.
    pub fn mark_cache_hit(&mut self) {
        self.cache_hit = true;
    }

    /// B2: fold one provider result's usage into this meta. Token/cost values
    /// SUM across contributing successful legs (hybrid/blend/research); `None`
    /// entries never overwrite an already-recorded value.
    pub fn set_usage(
        &mut self,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        total_tokens: Option<u64>,
        cost: Option<f64>,
    ) {
        self.input_tokens = add_opt(self.input_tokens, input_tokens);
        self.output_tokens = add_opt(self.output_tokens, output_tokens);
        self.total_tokens = add_opt(self.total_tokens, total_tokens);
        self.cost = match (self.cost, cost) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
    }

    /// Fold another attempt-batch meta into this one (multi-provider / multi-leg).
    pub fn absorb(&mut self, other: ExecMeta) {
        for s in other.providers_consulted {
            if !self.providers_consulted.iter().any(|x| x == &s) {
                self.providers_consulted.push(s);
            }
        }
        self.attempt_count = self.attempt_count.saturating_add(other.attempt_count);
        if other.had_success {
            self.key_id = other.key_id;
            self.node_id = other.node_id;
            self.had_success = true;
        } else if !self.had_success && other.key_id.is_some() {
            self.key_id = other.key_id;
            self.node_id = other.node_id;
        }
        self.cache_hit = self.cache_hit || other.cache_hit;
        self.set_usage(
            other.input_tokens,
            other.output_tokens,
            other.total_tokens,
            other.cost,
        );
    }
}

/// Fold ONE leg outcome's meta (success OR error) into the accumulator —
/// wraps the `match { Ok(o) => absorb(o.meta), Err(o) => absorb(o.meta) }`
/// idiom used by multi-leg executes (hybrid/blend), where the legs are still
/// referenced afterwards so the metas are cloned.
pub(crate) fn absorb<T, E>(
    meta: &mut ExecMeta,
    leg: &Result<ProductOutcome<T>, ProductOutcome<E>>,
) {
    match leg {
        Ok(o) => meta.absorb(o.meta.clone()),
        Err(o) => meta.absorb(o.meta.clone()),
    }
}

/// `None + x = x`, `Some(a) + Some(b) = Some(a + b)` (saturating for u64).
fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.saturating_add(y)),
        (a, b) => a.or(b),
    }
}

/// One observable step of a provider attempt / research phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// About to attempt a provider call.
    Attempt {
        service: String,
        attempt: u32,
        max: u32,
    },
    /// A retryable failure; about to retry the same provider.
    Retry {
        service: String,
        attempt: u32,
        reason: String,
    },
    /// Moving to the next provider in a fallback chain.
    Fallback {
        from: String,
        to: String,
        reason: String,
    },
    /// Research phase boundary (web / scrape / social).
    Phase { name: String, done: u32, total: u32 },
}

impl ProgressEvent {
    /// Human-readable one-liner used as the MCP progress message.
    pub fn message(&self) -> String {
        match self {
            Self::Attempt {
                service,
                attempt,
                max,
            } => {
                format!("{service} attempt {attempt}/{max}")
            }
            Self::Retry {
                service,
                attempt,
                reason,
            } => {
                format!("{service} attempt {attempt} failed, retrying: {reason}")
            }
            Self::Fallback { from, to, .. } => format!("{from} failed → {to}"),
            Self::Phase { name, done, total } => {
                format!("research: {name} {done}/{total}")
            }
        }
    }
}

/// Live, per-request snapshot of the in-flight [`ExecMeta`] (F10
/// attribution).
///
/// The `ExecMeta` of a running product call only exists INSIDE that call's
/// futures, so it is handed back on completion — which is exactly the value
/// the API-side deadline does NOT see: `run_with_deadline` drops the product
/// future when the budget elapses, and a 504 was therefore reported as
/// "service unknown, 0 attempts" even after the request had leased a key and
/// dialed a vendor. The product layer publishes a snapshot at every
/// attempt/lease/provider-call site instead, and the Elapsed arm reads
/// [`MetaSink::last`].
///
/// Deliberately a concrete type (not a `dyn` trait, unlike
/// [`ProgressSink`]): this is a one-slot, clone-on-write cell with no
/// implementation variance, and the sink is created and consumed inside one
/// process (the API request path).
#[derive(Debug, Default)]
pub struct MetaSink {
    inner: std::sync::Mutex<Option<ExecMeta>>,
}

impl MetaSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the current meta. Last writer wins — the newest attempt is the
    /// one a timeout must be attributed to.
    pub fn observe(&self, meta: &ExecMeta) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(meta.clone());
    }

    /// Most recently published snapshot (`None` when no attempt was recorded —
    /// e.g. the request never reached a provider).
    pub fn last(&self) -> Option<ExecMeta> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Outbound observer hook. Product emits; the API layer decides what to do.
pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: &ProgressEvent);
}

/// Default sink: discards. Used when `ProductCtx.progress` is `None`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn emit(&self, _event: &ProgressEvent) {}
}

/// Product free-fn return: wire `result` plus non-wire `meta`.
#[derive(Clone, Debug)]
pub struct ProductOutcome<T> {
    pub result: T,
    pub meta: ExecMeta,
}

impl<T> ProductOutcome<T> {
    pub fn new(result: T, meta: ExecMeta) -> Self {
        Self { result, meta }
    }

    pub fn ok(result: T) -> Self {
        Self {
            result,
            meta: ExecMeta::default(),
        }
    }

    pub fn map_result<U, F: FnOnce(T) -> U>(self, f: F) -> ProductOutcome<U> {
        ProductOutcome {
            result: f(self.result),
            meta: self.meta,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_attempt_last_success_wins_else_last() {
        let mut m = ExecMeta::default();
        m.note_attempt("tavily", 1, Some(10), false);
        assert_eq!(m.key_id, Some(1));
        assert_eq!(m.attempt_count, 1);
        m.note_attempt("firecrawl", 2, Some(11), true);
        assert_eq!(m.key_id, Some(2));
        m.note_attempt("exa", 3, None, false);
        // sticky last success
        assert_eq!(m.key_id, Some(2));
        assert_eq!(m.node_id, Some(11));
        assert_eq!(m.providers_csv().as_deref(), Some("tavily,firecrawl,exa"));
        assert_eq!(m.attempt_count, 3);
    }

    #[test]
    fn all_failures_keep_last_attempt() {
        let mut m = ExecMeta::default();
        m.note_attempt("tavily", 1, None, false);
        m.note_attempt("firecrawl", 2, Some(9), false);
        assert_eq!(m.key_id, Some(2));
        assert_eq!(m.node_id, Some(9));
        assert!(!m.had_success);
    }

    /// A timeout must be attributable to the vendor the request was actually
    /// on: the sink publishes each attempt, and `last()` is what the API
    /// deadline arm reads after dropping the product future.
    #[test]
    fn meta_sink_last_is_the_most_recent_attempt() {
        let sink = MetaSink::new();
        assert!(sink.last().is_none(), "nothing published yet");

        let mut m = ExecMeta::default();
        m.note_attempt("tavily", 1, Some(10), false);
        sink.observe(&m);
        m.note_attempt("firecrawl", 2, Some(11), false);
        sink.observe(&m);

        let last = sink.last().expect("a snapshot was published");
        assert_eq!(last.providers_consulted, vec!["tavily", "firecrawl"]);
        assert_eq!(last.attempt_count, 2);
        assert_eq!(last.key_id, Some(2));
        assert_eq!(last.node_id, Some(11));
    }

    /// An in-flight attempt (leased but not yet answered) is published too:
    /// this is the only case a 504 can catch, and it must name the vendor
    /// without inventing a completed attempt.
    #[test]
    fn note_attempt_pending_names_the_vendor_without_counting_the_attempt() {
        let mut m = ExecMeta::default();
        m.note_attempt_pending("xai", 5, None);
        assert_eq!(m.providers_consulted, vec!["xai"]);
        assert_eq!(m.key_id, Some(5));
        assert_eq!(m.attempt_count, 0, "an unfinished call is not an attempt");

        // The real record lands when the vendor answers.
        m.note_attempt("xai", 5, None, true);
        assert_eq!(m.attempt_count, 1);
        assert_eq!(m.providers_consulted, vec!["xai"], "recorded once");
    }

    /// A pending (in-flight) attempt must not steal a leg's sticky
    /// last-success key/node: a multi-leg request that succeeded on tavily and
    /// then timed out mid-call on exa must still report the tavily key, or the
    /// 504 event and the `usage_daily` rollup bill the wrong key.
    #[test]
    fn note_attempt_pending_never_overwrites_a_sticky_success() {
        let mut m = ExecMeta::default();
        m.note_attempt("tavily", 1, Some(10), true);
        assert!(m.had_success);
        // The next leg leases but never answers before the deadline fires.
        m.note_attempt_pending("exa", 2, Some(20));
        assert_eq!(
            m.key_id,
            Some(1),
            "an in-flight leg must not overwrite the last success"
        );
        assert_eq!(m.node_id, Some(10));
        // The vendor IS still recorded — that is what a timeout names.
        assert_eq!(m.providers_consulted, vec!["tavily", "exa"]);
        assert_eq!(m.attempt_count, 1, "an unfinished call is not an attempt");

        // And the negative: with no success yet, the pending write is allowed.
        let mut cold = ExecMeta::default();
        cold.note_attempt_pending("exa", 7, Some(8));
        assert_eq!(cold.key_id, Some(7));
        assert_eq!(cold.node_id, Some(8));
    }

    /// Snapshots are independent values: mutating the meta after `observe`
    /// must not retroactively change what was published, so the API reads a
    /// frozen point-in-time record.
    #[test]
    fn meta_sink_snapshots_are_frozen_copies() {
        let sink = MetaSink::new();
        let mut m = ExecMeta::default();
        m.note_attempt("tavily", 1, None, false);
        sink.observe(&m);
        m.note_attempt("exa", 2, None, false);
        let last = sink.last().expect("published");
        assert_eq!(last.attempt_count, 1, "later edits must not leak back");
        assert_eq!(last.providers_consulted, vec!["tavily"]);
    }

    #[test]
    fn providers_csv_none_when_empty() {
        assert!(ExecMeta::default().providers_csv().is_none());
    }

    #[test]
    fn set_usage_sums_and_never_overwrites_none() {
        let mut m = ExecMeta::default();
        assert!(m.input_tokens.is_none() && m.cost.is_none());
        m.set_usage(Some(10), Some(20), Some(30), Some(0.5));
        assert_eq!(m.input_tokens, Some(10));
        assert_eq!(m.output_tokens, Some(20));
        assert_eq!(m.total_tokens, Some(30));
        assert_eq!(m.cost, Some(0.5));
        // A second successful leg (hybrid/blend) sums.
        m.set_usage(Some(5), None, Some(15), Some(0.25));
        assert_eq!(m.input_tokens, Some(15), "input tokens summed");
        assert_eq!(m.output_tokens, Some(20), "None never overwrites");
        assert_eq!(m.total_tokens, Some(45));
        assert_eq!(m.cost, Some(0.75));
        // All-None fold is a no-op.
        m.set_usage(None, None, None, None);
        assert_eq!(m.input_tokens, Some(15));
    }

    #[test]
    fn absorb_folds_usage_and_cache_hit() {
        let mut a = ExecMeta::default();
        a.note_attempt("tavily", 1, None, true);
        let mut b = ExecMeta::default();
        b.note_attempt("xai", 2, None, true);
        b.set_usage(Some(3), Some(40), Some(43), Some(0.001));
        a.absorb(b);
        assert_eq!(a.providers_consulted, vec!["tavily", "xai"]);
        assert_eq!(a.attempt_count, 2);
        assert_eq!(a.input_tokens, Some(3));
        assert_eq!(a.cost, Some(0.001));
        assert!(!a.cache_hit, "no cache flag in the folded metas");

        let mut cached = ExecMeta::default();
        cached.mark_cache_hit();
        a.absorb(cached);
        assert!(a.cache_hit, "cache_hit propagates through absorb");
    }

    #[test]
    fn mark_cache_hit_flips_flag_only() {
        let mut m = ExecMeta::default();
        assert!(!m.cache_hit);
        m.mark_cache_hit();
        assert!(m.cache_hit);
        assert_eq!(m.attempt_count, 0, "no attempt recorded for a cache serve");
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn event_messages_render_one_liners() {
        assert_eq!(
            ProgressEvent::Attempt {
                service: "tavily".into(),
                attempt: 2,
                max: 3
            }
            .message(),
            "tavily attempt 2/3"
        );
        assert_eq!(
            ProgressEvent::Retry {
                service: "tavily".into(),
                attempt: 1,
                reason: "upstream 429".into()
            }
            .message(),
            "tavily attempt 1 failed, retrying: upstream 429"
        );
        assert_eq!(
            ProgressEvent::Fallback {
                from: "tavily".into(),
                to: "firecrawl".into(),
                reason: "exhausted".into()
            }
            .message(),
            "tavily failed → firecrawl"
        );
        assert_eq!(
            ProgressEvent::Phase {
                name: "scrape".into(),
                done: 2,
                total: 5
            }
            .message(),
            "research: scrape 2/5"
        );
    }

    #[test]
    fn noop_sink_discards() {
        NoopSink.emit(&ProgressEvent::Attempt {
            service: "x".into(),
            attempt: 1,
            max: 1,
        });
    }
}
