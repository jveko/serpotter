//! Background maintenance: re-enable stale keys, purge expired sessions, optional credit sync.

use std::sync::Arc;
use std::time::Duration;

use serpotter_db::Db;
use serpotter_outbound::ProxyPool;
use serpotter_providers::ProviderRegistry;
use tokio::task::JoinHandle;

const MAINT_PERIOD: Duration = Duration::from_secs(900); // 15m

/// Cadence of the standalone high-error-rate check. The window it samples is
/// [`ALERT_WINDOW_MINUTES`] (5m), so a 15-minute sampler misses a spike that
/// has already rolled out of the ring — the alert would fire up to three
/// windows late, or not at all for a burst that starts and ends inside one
/// tick. 60s keeps detection within one window without paging on noise
/// (the `ALERT_MIN_TOTAL` gate, not the cadence, does that work).
const ALERT_PERIOD: Duration = Duration::from_secs(60);

/// Smallest accepted re-enable window, in hours. This is deliberately an
/// alias of [`Db::REENABLE_MIN_HOURS`] and NOT a second literal: two
/// constants for one rule is exactly how the cron and the SQL drift apart.
const REENABLE_MIN_HOURS: i64 = Db::REENABLE_MIN_HOURS;

/// Spawn a 15-minute interval loop for key re-enable, expired-session purge,
/// and optional Tavily/Firecrawl credit sync when `CREDIT_SYNC_CRON=1`.
/// Returns a handle so the caller can abort the task on process shutdown.
///
/// The tokio interval's boot-time immediate tick is consumed eagerly, so the
/// first maintenance pass runs after one FULL period — no credit-sync storm
/// (`CREDIT_SYNC_CRON=1`) or purge burst on every process restart.
///
/// Alerting is NOT here: it is spawned separately by [`spawn_error_rate_alerts`]
/// because it needs a far shorter cadence.
pub fn spawn_maintenance(db: Db, providers: ProviderRegistry) -> JoinHandle<()> {
    spawn_maintenance_with_period(db, providers, MAINT_PERIOD)
}

/// Like [`spawn_maintenance`] with an explicit period (tests / tuning).
pub fn spawn_maintenance_with_period(
    db: Db,
    providers: ProviderRegistry,
    period: Duration,
) -> JoinHandle<()> {
    // Validate the re-enable windows ONCE at startup, not on every pass: a
    // misconfigured value would otherwise re-log the same warning 96 times a
    // day, and a warning that repeats forever stops being read. The clamped
    // values are logged at `info` so the effective windows are on the record
    // too.
    let hours = validate_reenable_hours();
    let node_hours = validate_node_reenable_hours();
    tracing::info!(
        hours,
        node_hours,
        "api key / node re-enable windows in effect"
    );
    let providers = Arc::new(providers);
    tokio::spawn(maintenance_loop(db, providers, period))
}

async fn maintenance_loop(db: Db, providers: Arc<ProviderRegistry>, period: Duration) {
    let mut tick = tokio::time::interval(period);
    // Consume the interval's immediate first tick so the first maintenance
    // pass happens after one full period (boot-time runs are unwanted: they
    // would sync every key against vendor usage limits and purge on restart).
    tick.tick().await;
    loop {
        tick.tick().await;
        run_maintenance_once(&db, &providers).await;
    }
}

/// The high-error-rate check on its OWN [`ALERT_PERIOD`] loop, spawned
/// alongside the maintenance loop. Sampling a 5-minute error window every 15
/// minutes was a cadence bug, not a tuning choice: a spike that clears
/// between two ticks is never seen at all, and one that persists is reported
/// up to three windows late. The boot-time immediate tick is consumed here
/// too, so a fresh process does not alert on its own cold (empty) window.
pub fn spawn_error_rate_alerts(events: crate::events::RequestEvents) -> JoinHandle<()> {
    spawn_error_rate_alerts_with_period(events, ALERT_PERIOD)
}

/// Like [`spawn_error_rate_alerts`] with an explicit cadence (tests).
pub fn spawn_error_rate_alerts_with_period(
    events: crate::events::RequestEvents,
    period: Duration,
) -> JoinHandle<()> {
    // The task runs on a WORKER thread, where a thread-local default
    // subscriber (as installed by `set_default`) is not visible. Capture the
    // ambient dispatcher here and re-install it inside the task, so the alert
    // is emitted through whatever the caller had configured rather than
    // silently falling back to the global default.
    let dispatcher = tracing::dispatcher::get_default(|d| d.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(period);
        tick.tick().await;
        loop {
            tick.tick().await;
            tracing::dispatcher::with_default(&dispatcher, || {
                alert_if_high_error_rate(&events.error_window)
            })
            .await;
        }
    })
}

/// Re-enable hours with the [`REENABLE_MIN_HOURS`] floor applied, SILENTLY.
/// This runs on every maintenance pass; the operator-facing warning is
/// [`validate_reenable_hours`]'s job, fired once at startup.
///
/// `0` does NOT mean "cron disabled" and is not treated as such: it would make
/// the SQL predicate `last_used_at < datetime('now')`, true for every idle
/// inactive row, i.e. fail@3 backoff silently OFF. A negative value is worse —
/// `'-' || '-1' || ' hours'` is a malformed modifier SQLite evaluates to NULL,
/// so the query matches nothing and the operator sees a healthy-looking "no
/// rows to re-enable".
fn reenable_hours() -> i64 {
    env_i64_or("KEY_REENABLE_AFTER_HOURS", 24).max(REENABLE_MIN_HOURS)
}

/// Warn ONCE, at startup, when `KEY_REENABLE_AFTER_HOURS` is below the floor,
/// and return the clamped value. Separate from [`reenable_hours`] so the
/// warning fires once instead of on all 96 daily passes.
pub(crate) fn validate_reenable_hours() -> i64 {
    let raw = env_i64_or("KEY_REENABLE_AFTER_HOURS", 24);
    if raw < REENABLE_MIN_HOURS {
        tracing::warn!(
            var = "KEY_REENABLE_AFTER_HOURS",
            raw_value = raw,
            floor = REENABLE_MIN_HOURS,
            "KEY_REENABLE_AFTER_HOURS below 1 is not a valid window (0 would disable \
             fail@3 backoff entirely, a negative value matches nothing); clamping to \
             the floor. It does NOT disable the re-enable cron"
        );
        return REENABLE_MIN_HOURS;
    }
    raw
}

/// Node twin of [`reenable_hours`]: `NODE_REENABLE_AFTER_HOURS` with the same
/// [`REENABLE_MIN_HOURS`] floor, applied SILENTLY on every pass.
fn node_reenable_hours() -> i64 {
    env_i64_or("NODE_REENABLE_AFTER_HOURS", 24).max(REENABLE_MIN_HOURS)
}

/// Node twin of [`validate_reenable_hours`]: warn ONCE, at startup, when
/// `NODE_REENABLE_AFTER_HOURS` is below the floor, and return the clamped
/// value. Same two failure modes as the key knob, unchanged by the node table:
/// `0` makes `disabled_at <= datetime('now')` true for every disabled node
/// (node fail@max backoff silently off), and a negative value forms
/// `datetime('now', '--1 hours')` → NULL → matches nothing (a silent no-op
/// that reads as "nothing to re-enable").
pub(crate) fn validate_node_reenable_hours() -> i64 {
    let raw = env_i64_or("NODE_REENABLE_AFTER_HOURS", 24);
    if raw < REENABLE_MIN_HOURS {
        tracing::warn!(
            var = "NODE_REENABLE_AFTER_HOURS",
            raw_value = raw,
            floor = REENABLE_MIN_HOURS,
            "NODE_REENABLE_AFTER_HOURS below 1 is not a valid window (0 would disable \
             node fail@max backoff entirely, a negative value matches nothing); \
             clamping to the floor. It does NOT disable the re-enable cron"
        );
        return REENABLE_MIN_HOURS;
    }
    raw
}

/// One maintenance pass: re-enable stale keys/nodes, purge expired
/// admin_sessions, optionally sync credits. Extracted from the loop so
/// tests can drive a single pass deterministically.
///
/// No alerting here: the high-error-rate check moved to its own
/// [`ALERT_PERIOD`] loop ([`spawn_error_rate_alerts`]) because it samples a
/// 5-minute window and this loop only ticks every 15 minutes.
async fn run_maintenance_once(db: &Db, providers: &ProviderRegistry) {
    let hours = reenable_hours();
    let node_hours = node_reenable_hours();
    match db.reenable_stale_keys(hours).await {
        Ok(n) if n > 0 => tracing::info!(n, hours, "re-enabled stale api keys"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "reenable_stale_keys failed"),
    }
    match db.reenable_stale_nodes(node_hours).await {
        Ok(n) if n > 0 => {
            tracing::info!(n, hours = node_hours, "re-enabled stale outbound nodes")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "reenable_stale_nodes failed"),
    }
    match db.purge_expired_admin_sessions().await {
        Ok(n) if n > 0 => tracing::info!(n, "purged expired admin_sessions"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge_expired_admin_sessions failed"),
    }

    // B1: purge expired query-cache rows (cache_get filters them anyway; this
    // keeps the table bounded).
    match db.purge_expired_cache().await {
        Ok(n) if n > 0 => tracing::info!(n, "purged expired query-cache rows"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "purge_expired_cache failed"),
    }

    // B5: keep the key-pool-depth gauge fresh between ticks.
    crate::metrics::refresh_key_pool_depth(db).await;

    // Off by default — avoid hammering vendor usage APIs every 15m.
    let credit_sync = std::env::var("CREDIT_SYNC_CRON")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    if credit_sync {
        match crate::credit_sync::sync_credits_for_services(
            db,
            providers,
            &["tavily", "firecrawl"],
            crate::credit_sync::MAX_KEYS_PER_SERVICE,
        )
        .await
        {
            Ok(r) if r.synced > 0 || r.errors > 0 || r.skipped > 0 => {
                tracing::info!(
                    synced = r.synced,
                    errors = r.errors,
                    skipped = r.skipped,
                    "cron credit sync finished (keys over the per-pass cap wait for \
                     the next tick)"
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "cron credit sync failed"),
        }
    }
}

/// Alert step, extracted so tests can drive it without the rest of the
/// maintenance pass: `tracing::error!` when the window ratio is exceeded,
/// then the optional webhook POST.
async fn alert_if_high_error_rate(window: &crate::events::ErrorWindow) {
    let Some(stats) = check_error_rate(window) else {
        return;
    };
    tracing::error!(
        total = stats.total,
        errors = stats.errors,
        error_rate = stats.error_rate(),
        "high request error rate over the last 5 minutes"
    );
    fire_alert(stats);
}

/// Read an integer cron env var, warning (never silently) when the value is set
/// but unparseable. Missing var → `default` without a warning.
pub(crate) fn env_i64_or(key: &str, default: i64) -> i64 {
    match std::env::var(key) {
        Ok(raw) => match raw.parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    var = key,
                    raw_value = %raw,
                    default,
                    "cron env value is not a valid integer; using default"
                );
                default
            }
        },
        Err(_) => default,
    }
}

// --- B15: high-error-rate alerting ------------------------------------------

/// Alert window: the last 5 minutes of request events.
pub(crate) const ALERT_WINDOW_MINUTES: i64 = 5;
/// Minimum requests in the window before the ratio is trusted at all.
///
/// Sizing rationale, not an arbitrary round number: at 20 requests, a >50%
/// error rate means ≥11 failures, and the sampling noise on a ratio that
/// coarse is small enough to act on. Below it the ratio is dominated by
/// one or two unlucky requests — `1/2` "is" a 50% error rate — so a
/// low-traffic deployment intentionally does NOT page; a
/// single-user instance with 3 requests in 5 minutes is healthy, not an
/// incident. Raising [`ALERT_PERIOD`]'s frequency does not change this:
/// the gate is about statistical meaning, not about when we look.
pub(crate) const ALERT_MIN_TOTAL: i64 = 20;
/// Alert when `errors / total > 0.5` (strictly greater — exactly half is not
/// "high error rate").
pub(crate) const ALERT_ERROR_RATIO: f64 = 0.5;

/// Computed 5-minute error-rate snapshot, ready to alert on.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ErrorRateStats {
    pub total: i64,
    pub errors: i64,
}

impl ErrorRateStats {
    pub fn error_rate(&self) -> f64 {
        if self.total <= 0 {
            0.0
        } else {
            self.errors as f64 / self.total as f64
        }
    }
}

/// Compute the alert stats from the in-memory error window, or `None` when
/// the rate is below the threshold. 2xx = success; everything else (401, 429,
/// 499, 5xx, …) counts as an error, matching the metrics `status_class`
/// semantics. A cold window (empty after restart) never fires: it needs
/// [`ALERT_MIN_TOTAL`] requests to accumulate first.
fn check_error_rate(window: &crate::events::ErrorWindow) -> Option<ErrorRateStats> {
    let (total, errors) = window.counts(ALERT_WINDOW_MINUTES);
    let stats = ErrorRateStats { total, errors };
    (stats.total >= ALERT_MIN_TOTAL && stats.error_rate() > ALERT_ERROR_RATIO).then_some(stats)
}

/// Fire-and-forget webhook POST when `ADMIN_ALERT_URL` is set: JSON body
/// `{errorRate, total, errors, ts}` with a 5s client timeout. The
/// `tracing::error!` in `alert_if_high_error_rate` already fired; the webhook is
/// optional extra signal, so every failure here is only a WARN.
fn fire_alert(stats: ErrorRateStats) {
    let Some(url) = std::env::var("ADMIN_ALERT_URL")
        .ok()
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let body = serde_json::json!({
        "errorRate": stats.error_rate(),
        "total": stats.total,
        "errors": stats.errors,
        "ts": ts,
    });
    tokio::spawn(async move {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "alert webhook client build failed");
                return;
            }
        };
        match client.post(&url).json(&body).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                tracing::warn!(
                    status = resp.status().as_u16(),
                    "admin alert webhook rejected the payload"
                );
            }
            Err(e) => tracing::warn!(error = %e, "admin alert webhook POST failed"),
        }
    });
}

// --- daily key health probe --------------------------------------------------

/// Default probe hour (UTC) when `KEY_PROBE_AT_HOUR` is unset or outside 0..=23.
const PROBE_AT_HOUR_DEFAULT: i64 = 4;

/// Spawn the daily key health probe loop, gated on `KEY_PROBE_CRON`
/// (`1`/`true`/`yes`, case-insensitive; OFF by default so a default deploy
/// behaves exactly as today — no probe traffic). Gate off → an
/// immediately-completing no-op task; the caller still aborts it at shutdown,
/// which is harmless.
///
/// The pass itself is `serpotter_product::probe_due_keys` — this module owns
/// only the gate, hour, stagger, loop, and completion log.
pub fn spawn_key_probes(
    db: Db,
    providers: ProviderRegistry,
    outbound: Arc<ProxyPool>,
) -> JoinHandle<()> {
    if !probe_cron_enabled_from(std::env::var("KEY_PROBE_CRON").ok().as_deref()) {
        tracing::info!("daily key probe is disabled (set KEY_PROBE_CRON=1|true|yes)");
        return tokio::spawn(async {});
    }
    let hour = validate_probe_at_hour();
    // Pacing knob, not a correctness floor: a negative stagger clamps to 0
    // silently (the per-row sleep in `probe_due_keys` then becomes a no-op).
    let stagger = Duration::from_millis(env_i64_or("KEY_PROBE_STAGGER_MS", 300).max(0) as u64);
    tokio::spawn(probe_loop(db, providers, outbound, hour, stagger))
}

/// The `KEY_PROBE_CRON` gate, pure (no env read → no test races): true only
/// for `1`/`true`/`yes`, case-insensitive — the same REQUIRE-style matching
/// `main.rs` uses for `REQUIRE_OUTBOUND_PROXY`.
fn probe_cron_enabled_from(raw: Option<&str>) -> bool {
    matches!(
        raw.map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes")
    )
}

/// `KEY_PROBE_AT_HOUR` → probe hour (UTC), pure: valid `0..=23` passes
/// through, anything else falls back to [`PROBE_AT_HOUR_DEFAULT`]. The
/// warn-ONCE lives in [`validate_probe_at_hour`], mirroring the
/// `validate_reenable_hours` / `reenable_hours` split above — a warning here
/// would fire on all 24 daily passes.
fn probe_at_hour_from(raw: i64) -> i64 {
    if (0..=23).contains(&raw) {
        raw
    } else {
        PROBE_AT_HOUR_DEFAULT
    }
}

/// Warn ONCE, at startup, when `KEY_PROBE_AT_HOUR` is outside `0..=23`, and
/// return the effective hour. (An unparseable value already warned inside
/// [`env_i64_or`] and arrives here as the default — no double warning.)
fn validate_probe_at_hour() -> i64 {
    let raw = env_i64_or("KEY_PROBE_AT_HOUR", PROBE_AT_HOUR_DEFAULT);
    let hour = probe_at_hour_from(raw);
    if hour != raw {
        tracing::warn!(
            var = "KEY_PROBE_AT_HOUR",
            raw_value = raw,
            default = PROBE_AT_HOUR_DEFAULT,
            "KEY_PROBE_AT_HOUR must be an hour of day in 0..=23 (UTC); \
             falling back to the default"
        );
    }
    hour
}

/// Seconds until the next `hour` mark of the UTC day, pure. Exactly on the
/// hour it returns a FULL day rather than 0, so the loop can never
/// zero-sleep spin. `now_secs_of_day` is epoch-seconds mod 86400 — UTC,
/// matching SQLite `date('now')`'s clock.
fn secs_until_utc_hour(now_secs_of_day: u64, hour: i64) -> Duration {
    let target = hour.clamp(0, 23) as u64 * 3600;
    let now = now_secs_of_day % (24 * 3600);
    let wait = (target + 24 * 3600 - now) % (24 * 3600);
    Duration::from_secs(if wait == 0 { 24 * 3600 } else { wait })
}

/// Current seconds of the UTC day, from the same clock SQLite's
/// `datetime('now')` stamps key rows with.
fn now_secs_of_day() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() % (24 * 3600))
        .unwrap_or(0)
}

/// The probe loop: the FIRST pass runs immediately (boot picks up due keys),
/// then it sleeps to the next UTC `hour`. A failed pass warns and still
/// sleeps — errors must never turn into a hot spin.
async fn probe_loop(
    db: Db,
    providers: ProviderRegistry,
    outbound: Arc<ProxyPool>,
    hour: i64,
    stagger: Duration,
) {
    loop {
        let started = std::time::Instant::now();
        match serpotter_product::probe_due_keys(&db, &providers, &outbound, stagger).await {
            Ok(stats) => tracing::info!(
                probed = stats.probed,
                ok = stats.ok,
                deleted_401 = stats.deleted_401,
                banned_deleted = stats.banned_deleted,
                banned_suspended = stats.banned_suspended,
                auth_fail = stats.auth_fail,
                rate_limited = stats.rate_limited,
                drained = stats.drained,
                unchanged = stats.unchanged,
                aborted = stats.aborted,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "daily key probe pass complete"
            ),
            Err(e) => tracing::warn!(error = %e, "daily key probe pass failed"),
        }
        tokio::time::sleep(secs_until_utc_hour(now_secs_of_day(), hour)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_providers::{ExaClient, FirecrawlClient, TavilyClient, XaiClient};

    /// Serializes process-env mutation so parallel tests never race set/remove.
    static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Serializes global-subscriber swaps (`set_default`) against other tests
    /// that capture tracing output.
    static CAPTURE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// Test-only capture sink for WARN+ events (Arc-owned buffer, no leak).
    #[derive(Clone, Default)]
    struct CaptureSink(Arc<parking_lot::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureSink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_warns(f: impl FnOnce()) -> String {
        capture_at(tracing::Level::WARN, f)
    }

    fn capture_at(level: tracing::Level, f: impl FnOnce()) -> String {
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(level)
            .with_ansi(false) // CI runners emit ANSI escapes; assertions need plain text
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let guard = sink.0.lock();
        String::from_utf8_lossy(&guard).into_owned()
    }

    /// Seed the error window with `n` requests of one status, all inside the
    /// alert window (a fixed minute one minute back).
    fn seed_error_window(window: &crate::events::ErrorWindow, status: i64, n: usize) {
        let minute = crate::events::now_minute() - 1;
        for _ in 0..n {
            window.record_at(status, minute);
        }
    }

    #[test]
    fn invalid_cron_env_warns_and_defaults() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("KEY_REENABLE_AFTER_HOURS", "not-a-number");
        let text = capture_warns(|| {
            assert_eq!(env_i64_or("KEY_REENABLE_AFTER_HOURS", 24), 24);
        });
        std::env::remove_var("KEY_REENABLE_AFTER_HOURS");
        assert!(
            text.contains("KEY_REENABLE_AFTER_HOURS"),
            "warn must name the var: {text}"
        );
        assert!(
            text.contains("not-a-number"),
            "warn must carry the raw offending value: {text}"
        );
    }

    #[test]
    fn missing_cron_env_defaults_without_warning() {
        let text = capture_warns(|| {
            assert_eq!(env_i64_or("SERPOTTER_TEST_UNSET_VAR", 42), 42);
        });
        assert!(
            text.is_empty(),
            "no warn expected for a missing var: {text}"
        );
    }

    // --- B5: re-enable floor + standalone alert cadence ---------------------

    /// `KEY_REENABLE_AFTER_HOURS=0` is not "cron disabled" — fed straight into
    /// the SQL it makes the predicate true for every idle inactive row, i.e.
    /// fail@3 backoff silently off. The floor must be applied AND named.
    #[test]
    fn reenable_hours_zero_warns_and_clamps_to_the_floor() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("KEY_REENABLE_AFTER_HOURS", "0");
        let text = capture_warns(|| {
            assert_eq!(validate_reenable_hours(), REENABLE_MIN_HOURS, "0 clamps");
        });
        std::env::remove_var("KEY_REENABLE_AFTER_HOURS");
        assert!(
            text.contains("KEY_REENABLE_AFTER_HOURS"),
            "the operator must be told which var was wrong: {text}"
        );
        assert!(
            text.contains("does NOT disable"),
            "the warn must say 0 is not a disable switch: {text}"
        );
    }

    /// Same for a negative value, which used to build
    /// `datetime('now', '--1 hours')` → NULL → a silent match-nothing no-op.
    #[test]
    fn reenable_hours_negative_warns_and_clamps_to_the_floor() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("KEY_REENABLE_AFTER_HOURS", "-1");
        let hours = validate_reenable_hours();
        assert_eq!(hours, REENABLE_MIN_HOURS);
        std::env::remove_var("KEY_REENABLE_AFTER_HOURS");
    }

    /// A valid window passes through untouched, with no floor warning.
    #[test]
    fn reenable_hours_valid_value_is_untouched_and_silent() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("KEY_REENABLE_AFTER_HOURS", "6");
        let text = capture_warns(|| {
            assert_eq!(validate_reenable_hours(), 6);
        });
        std::env::remove_var("KEY_REENABLE_AFTER_HOURS");
        assert!(text.is_empty(), "a valid window must not warn: {text}");
    }

    /// The per-pass path clamps to the same floor but must stay SILENT: a
    /// startup warning re-logged 96 times a day is a warning nobody reads.
    #[test]
    fn per_pass_clamp_stays_silent_so_the_warning_is_not_repeated() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("KEY_REENABLE_AFTER_HOURS", "0");
        let text = capture_warns(|| {
            assert_eq!(reenable_hours(), REENABLE_MIN_HOURS);
            assert_eq!(reenable_hours(), REENABLE_MIN_HOURS);
        });
        std::env::remove_var("KEY_REENABLE_AFTER_HOURS");
        assert!(
            text.is_empty(),
            "only the startup validation may warn, not every 15-minute pass: {text}"
        );
    }

    // --- node re-enable floor (same rule, other knob) ----------------------

    /// The node knob is the same failure mode on a different table:
    /// `NODE_REENABLE_AFTER_HOURS=0` makes `disabled_at <= datetime('now')`
    /// true for every disabled node, i.e. node fail@max backoff silently off.
    /// The floor must be applied AND the warn must name the NODE var, not the
    /// key one — otherwise the operator edits the wrong knob.
    #[test]
    fn node_reenable_hours_zero_warns_and_clamps_to_the_floor() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("NODE_REENABLE_AFTER_HOURS", "0");
        let text = capture_warns(|| {
            assert_eq!(
                validate_node_reenable_hours(),
                REENABLE_MIN_HOURS,
                "0 clamps"
            );
        });
        std::env::remove_var("NODE_REENABLE_AFTER_HOURS");
        assert!(
            text.contains("NODE_REENABLE_AFTER_HOURS"),
            "the operator must be told which var was wrong: {text}"
        );
        assert!(
            text.contains("does NOT disable"),
            "the warn must say 0 is not a disable switch: {text}"
        );
    }

    /// Same for a negative value, which used to build
    /// `datetime('now', '--1 hours')` → NULL → a silent match-nothing no-op.
    #[test]
    fn node_reenable_hours_negative_warns_and_clamps_to_the_floor() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("NODE_REENABLE_AFTER_HOURS", "-1");
        let hours = validate_node_reenable_hours();
        assert_eq!(hours, REENABLE_MIN_HOURS);
        std::env::remove_var("NODE_REENABLE_AFTER_HOURS");
    }

    /// A valid node window passes through untouched, with no floor warning.
    #[test]
    fn node_reenable_hours_valid_value_is_untouched_and_silent() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("NODE_REENABLE_AFTER_HOURS", "6");
        let text = capture_warns(|| {
            assert_eq!(validate_node_reenable_hours(), 6);
        });
        std::env::remove_var("NODE_REENABLE_AFTER_HOURS");
        assert!(text.is_empty(), "a valid window must not warn: {text}");
    }

    /// The per-pass node path clamps to the same floor but must stay SILENT:
    /// only the startup validation may warn, not all 96 daily passes.
    #[test]
    fn node_per_pass_clamp_stays_silent_so_the_warning_is_not_repeated() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("NODE_REENABLE_AFTER_HOURS", "0");
        let text = capture_warns(|| {
            assert_eq!(node_reenable_hours(), REENABLE_MIN_HOURS);
            assert_eq!(node_reenable_hours(), REENABLE_MIN_HOURS);
        });
        std::env::remove_var("NODE_REENABLE_AFTER_HOURS");
        assert!(
            text.is_empty(),
            "only the startup validation may warn, not every 15-minute pass: {text}"
        );
    }

    /// One rule, one floor: keys and nodes must not drift into separate
    /// limits (the drift this whole clamp exists to prevent).
    #[test]
    fn node_and_key_windows_share_one_floor() {
        assert_eq!(REENABLE_MIN_HOURS, Db::REENABLE_MIN_HOURS);
        let _guard = ENV_LOCK.lock();
        std::env::set_var("KEY_REENABLE_AFTER_HOURS", "0");
        std::env::set_var("NODE_REENABLE_AFTER_HOURS", "0");
        assert_eq!(reenable_hours(), node_reenable_hours());
        std::env::remove_var("KEY_REENABLE_AFTER_HOURS");
        std::env::remove_var("NODE_REENABLE_AFTER_HOURS");
    }

    /// The alert samples a 5-minute window, so its own cadence must be well
    /// inside one — and far shorter than the 15-minute maintenance tick it
    /// used to ride along with.
    #[test]
    fn alert_cadence_fits_inside_its_own_window() {
        assert_eq!(ALERT_PERIOD, Duration::from_secs(60));
        assert!(
            ALERT_PERIOD.as_secs() < (ALERT_WINDOW_MINUTES as u64) * 60,
            "a cadence at or beyond the window length misses bursts entirely"
        );
        assert!(
            ALERT_PERIOD < MAINT_PERIOD,
            "the alert must be independent of the 15-minute tick"
        );
    }

    /// Pins the deliberate floor. Raising or lowering it changes who gets
    /// paged, so it must be a conscious edit — and the reasoning lives in the
    /// constant's doc comment, not in a changelog nobody reads.
    #[test]
    fn alert_min_total_stays_at_the_deliberate_floor() {
        assert_eq!(ALERT_MIN_TOTAL, 20);
        // The floor only means something against a window; keep them coherent.
        assert_eq!(ALERT_WINDOW_MINUTES, 5);
    }

    /// The alert loop must sample on its own schedule, not the maintenance
    /// one: a 5-minute window sampled every 15 minutes sees at most a third
    /// of a spike (and misses short ones entirely).
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // CAPTURE_LOCK deliberately serializes the subscriber swap
    async fn alert_loop_fires_on_its_own_short_cadence() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let (events, _writer) = crate::events::RequestEvents::new(db.clone());
        seed_error_window(&events.error_window, 200, 5);
        seed_error_window(&events.error_window, 503, 25);

        let _capture_guard = CAPTURE_LOCK.lock();
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::ERROR)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let handle = spawn_error_rate_alerts_with_period(events, Duration::from_millis(50));
        // The maintenance period is never involved here; the loop must act on
        // its own 50ms tick.
        let mut fired = false;
        for _ in 0..80 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if String::from_utf8_lossy(&sink.0.lock()).contains("high request error rate") {
                fired = true;
                break;
            }
        }
        drop(_guard);
        handle.abort();
        assert!(
            fired,
            "the alert loop must sample independently of maintenance"
        );
    }

    async fn count_admin_sessions(db: &Db) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM admin_sessions")
            .fetch_one(db.pool())
            .await
            .expect("count admin_sessions")
    }

    /// F55: the maintenance loop must NOT run at boot. With the boot-time
    /// immediate tick consumed, an expired admin_session inserted before spawn
    /// survives the first milliseconds and is purged only after one full period.
    #[tokio::test]
    async fn maintenance_first_tick_is_consumed_not_immediate() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let user = db
            .insert_admin_user("admin", "$argon2id$placeholder")
            .await
            .expect("insert admin user");
        db.insert_admin_session("boot-stale", user.id, "2000-01-01 00:00:00")
            .await
            .expect("insert stale session");
        let providers = ProviderRegistry::with_clients(
            TavilyClient::new("http://127.0.0.1:9"),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );
        let handle =
            spawn_maintenance_with_period(db.clone(), providers, Duration::from_millis(60));

        // Well before the first period: no maintenance pass has run.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            count_admin_sessions(&db).await,
            1,
            "maintenance must not run at boot (immediate tick consumed)"
        );

        // After one full period the first real tick purges the stale row.
        let mut purged = false;
        for _ in 0..80 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if count_admin_sessions(&db).await == 0 {
                purged = true;
                break;
            }
        }
        assert!(purged, "first period must run the maintenance pass");
        handle.abort();
    }

    // --- B15: high-error-rate alerting ---------------------------------------

    #[test]
    fn error_rate_above_threshold_triggers_alert() {
        let window = crate::events::ErrorWindow::new();
        // 10 ok + 20 errors over 30 requests → ratio 0.667 > 0.5 and total >= 20.
        seed_error_window(&window, 200, 10);
        seed_error_window(&window, 500, 20);
        let stats = check_error_rate(&window).expect("alert must fire");
        assert_eq!(stats.total, 30);
        assert_eq!(stats.errors, 20);
        assert!((stats.error_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn error_rate_below_threshold_stays_silent() {
        let window = crate::events::ErrorWindow::new();
        // 20 ok + 10 errors → ratio 1/3, no alert.
        seed_error_window(&window, 200, 20);
        seed_error_window(&window, 500, 10);
        assert!(check_error_rate(&window).is_none(), "no alert below ratio");
    }

    #[test]
    fn error_rate_respects_min_total_gate() {
        let window = crate::events::ErrorWindow::new();
        // 5 requests, ALL errors: ratio 1.0 but total < 20 → no alert.
        seed_error_window(&window, 500, 5);
        assert!(
            check_error_rate(&window).is_none(),
            "tiny noisy sample must not alert"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // CAPTURE_LOCK deliberately serializes the whole capture window
    async fn alert_fires_tracing_error_when_triggered() {
        let window = crate::events::ErrorWindow::new();
        seed_error_window(&window, 200, 10);
        seed_error_window(&window, 503, 20);

        // Global subscriber swap serialized against parallel capture tests.
        let _capture_guard = CAPTURE_LOCK.lock();
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false) // CI runners emit ANSI escapes; assertions need plain text
            .with_max_level(tracing::Level::ERROR)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        // Drive the real alert step (ADMIN_ALERT_URL unset → log only).
        alert_if_high_error_rate(&window).await;
        drop(_guard);
        let text = String::from_utf8_lossy(&sink.0.lock()).into_owned();
        assert!(
            text.contains("high request error rate"),
            "error! must fire above threshold: {text}"
        );
        assert!(
            text.contains("error_rate=0.666"),
            "carries the ratio: {text}"
        );
    }

    /// One-shot loopback HTTP server that captures the alert POST body.
    fn spawn_alert_listener() -> (String, std::sync::mpsc::Receiver<serde_json::Value>) {
        use std::io::{Read, Write};
        let (tx, rx) = std::sync::mpsc::channel();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 8192];
            let mut read = 0;
            // Read headers first (ends at \r\n\r\n).
            while !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") && read < buf.len() {
                match stream.read(&mut buf[read..]) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => read += n,
                }
            }
            let head_end = buf[..read]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|p| p + 4)
                .unwrap_or(read);
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let content_length: usize = head
                .lines()
                // reqwest sends lowercase header names; match case-insensitively.
                .find_map(|l| {
                    let lower = l.to_ascii_lowercase();
                    lower
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            // Read the full body (headers + body already buffered, top up if short).
            while read < head_end + content_length && read < buf.len() {
                match stream.read(&mut buf[read..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => read += n,
                }
            }
            let body = &buf[head_end..head_end + content_length.min(read - head_end)];
            let _ = tx.send(serde_json::from_slice(body).unwrap_or_default());
            let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            let _ = stream.write_all(resp.as_bytes());
        });
        (format!("http://{addr}"), rx)
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // ENV_LOCK deliberately serializes env mutation across the test
    async fn fire_alert_posts_json_to_webhook() {
        let _guard = ENV_LOCK.lock();
        let (url, rx) = spawn_alert_listener();
        std::env::set_var("ADMIN_ALERT_URL", url);

        fire_alert(ErrorRateStats {
            total: 40,
            errors: 30,
        });

        // The POST is fire-and-forget: poll until the loopback server replies.
        let mut received = None;
        for _ in 0..100 {
            match rx.try_recv() {
                Ok(v) => {
                    received = Some(v);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(_) => break,
            }
        }
        std::env::remove_var("ADMIN_ALERT_URL");
        drop(_guard);

        let v = received.expect("alert webhook must receive the payload");
        assert!((v["errorRate"].as_f64().unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(v["total"], 40);
        assert_eq!(v["errors"], 30);
        assert!(
            v["ts"].as_i64().unwrap() > 1_600_000_000,
            "ts is unix seconds"
        );
        // The payload is a cross-system contract (the operator's webhook
        // consumer), so pin the EXACT key set: no silent extras, no
        // snake_case drift from a struct field rename.
        let mut keys: Vec<&str> = v
            .as_object()
            .expect("webhook body is a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["errorRate", "errors", "total", "ts"],
            "alert webhook key set must stay exactly this"
        );
    }

    #[tokio::test]
    async fn fire_alert_without_url_does_not_hang_or_panic() {
        let _guard = ENV_LOCK.lock();
        std::env::remove_var("ADMIN_ALERT_URL");
        // Must return synchronously (no task, no network attempt).
        fire_alert(ErrorRateStats {
            total: 30,
            errors: 20,
        });
    }

    // --- daily key health probe helpers --------------------------------------

    #[test]
    fn probe_gate_only_true_for_on_values() {
        assert!(!probe_cron_enabled_from(None));
        assert!(!probe_cron_enabled_from(Some("")));
        assert!(!probe_cron_enabled_from(Some("0")));
        assert!(!probe_cron_enabled_from(Some("yes!")));
        assert!(probe_cron_enabled_from(Some("1")));
        assert!(probe_cron_enabled_from(Some("true")));
        assert!(probe_cron_enabled_from(Some("YES")));
    }

    #[test]
    fn probe_at_hour_valid_passthrough_out_of_range_falls_back() {
        assert_eq!(probe_at_hour_from(0), 0);
        assert_eq!(probe_at_hour_from(4), 4);
        assert_eq!(probe_at_hour_from(23), 23);
        // Out of 0..=23 falls back to the default 4; the warn-ONCE lives in
        // validate_probe_at_hour (mirrors validate_reenable_hours' split).
        assert_eq!(probe_at_hour_from(-1), 4);
        assert_eq!(probe_at_hour_from(25), 4);
    }

    #[test]
    fn secs_until_utc_hour_math() {
        assert_eq!(secs_until_utc_hour(3 * 3600, 4), Duration::from_secs(3600));
        assert_eq!(
            secs_until_utc_hour(5 * 3600, 4),
            Duration::from_secs(23 * 3600)
        );
        // Exactly on the hour: a FULL day, never a zero-sleep busy loop.
        assert_eq!(secs_until_utc_hour(4 * 3600, 4), Duration::from_secs(86400));
    }
}
