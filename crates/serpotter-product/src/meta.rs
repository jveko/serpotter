//! Non-wire execution metadata for request_log / spans (Approach 2 path A).

/// Accumulated per client call; never serialized on wire DTOs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecMeta {
    pub strategy: Option<String>,
    pub providers_consulted: Vec<String>,
    pub attempt_count: u32,
    pub key_id: Option<i64>,
    pub node_id: Option<i64>,
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

    /// Comma-separated, no spaces, first-seen order. `None` if empty.
    pub fn providers_csv(&self) -> Option<String> {
        if self.providers_consulted.is_empty() {
            None
        } else {
            Some(self.providers_consulted.join(","))
        }
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

    #[test]
    fn providers_csv_none_when_empty() {
        assert!(ExecMeta::default().providers_csv().is_none());
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
