//! Soft-fail credit sync for provider keys (admin + optional cron).
//! Tavily/Firecrawl: real usage endpoints. Exa/xAI: no reliable public usage API —
//! counting as soft errors only (never write fake credits, never deactivate).
//!
//! Throttled: Tavily's `GET /usage` allows ~10 calls / 10 min, so a pass never
//! fetches more than [`MAX_KEYS_PER_SERVICE`] keys per service, and the keys
//! beyond the cap are reported as skipped rather than silently dropped (the
//! list is ordered never-synced-first, so the next pass picks them up).

use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;

/// Per-service ceiling on vendor usage calls in ONE sync pass. Tavily's
/// `/usage` documents ~10 requests / 10 minutes; staying at or under that in a
/// single pass keeps a multi-key pool from 429-ing the usage API itself.
/// Firecrawl has no such documented cap but is bounded by the same rule — a
/// bound that holds for the tightest vendor is the one worth having.
pub const MAX_KEYS_PER_SERVICE: usize = 10;

/// Keys dropped by [`MAX_KEYS_PER_SERVICE`] in this pass. Reported so the
/// admin response and the cron log can say "there is more to do" instead of
/// presenting a capped pass as a complete one.
#[derive(Debug, Clone)]
pub struct SyncKeyResult {
    pub id: i64,
    pub ok: bool,
    pub remaining: Option<i64>,
    pub limit: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SyncCreditsReport {
    pub service: String,
    pub synced: i64,
    pub errors: i64,
    /// Active keys not fetched this pass because the per-service cap was hit.
    pub skipped: i64,
    pub results: Vec<SyncKeyResult>,
}

/// Number of entries a cap of `cap` removes from a list of `total`.
pub(crate) fn skipped_count(total: usize, cap: usize) -> i64 {
    total.saturating_sub(cap) as i64
}

/// Sync active keys for `services` (`tavily`/`firecrawl` real usage; `exa`/`xai` soft-error only).
/// Soft-fail per key: never sets active=0 on fetch/DB error.
///
/// At most [`MAX_KEYS_PER_SERVICE`] keys per service are contacted per call
/// (see the module docs for the vendor cap this honors); the remainder is
/// counted in [`SyncCreditsReport::skipped`].
pub async fn sync_credits_for_services(
    db: &Db,
    providers: &ProviderRegistry,
    services: &[&str],
    cap: usize,
) -> Result<SyncCreditsReport, serpotter_db::DbError> {
    let report_service = if services.len() == 1 {
        services[0].to_string()
    } else {
        "all".to_string()
    };

    let mut synced: i64 = 0;
    let mut errors: i64 = 0;
    let mut skipped: i64 = 0;
    let mut results: Vec<SyncKeyResult> = Vec::new();

    for service in services {
        let keys = match db.list_active_keys_for_service(service).await {
            Ok(keys) => keys,
            // Never abort the whole batch on a per-service DB error: warn,
            // count it as one error in the report, and continue.
            Err(e) => {
                tracing::warn!(
                    %service,
                    error = %e,
                    "list_active_keys_for_service failed; continuing with next service"
                );
                errors += 1;
                results.push(SyncKeyResult {
                    id: 0,
                    ok: false,
                    remaining: None,
                    limit: None,
                    error: Some(format!("key list failed: {e}")),
                });
                continue;
            }
        };
        // The list is ordered never-synced-first (then least-recently-synced),
        // so the cap defers work rather than starving a fixed subset.
        skipped += skipped_count(keys.len(), cap);
        for key in keys.into_iter().take(cap) {
            let http = providers.direct_client();
            let fetch = match *service {
                "tavily" => providers.tavily.fetch_usage(&http, &key.key).await,
                "firecrawl" => providers.firecrawl.fetch_usage(&http, &key.key).await,
                // No documented stable usage endpoint — honest soft-fail, no credit write.
                "exa" | "xai" => Err(serpotter_providers::ProviderError::Upstream {
                    provider: (*service).into(),
                    status: 501,
                    body: "usage sync not supported for this provider".into(),
                }),
                _ => continue,
            };

            match fetch {
                Ok(snap) => {
                    if let Err(e) = db
                        .update_api_key_usage(key.id, snap.remaining, snap.limit)
                        .await
                    {
                        errors += 1;
                        tracing::warn!(
                            key_id = key.id,
                            error = %e,
                            "update_api_key_usage failed; counting as sync error"
                        );
                        results.push(SyncKeyResult {
                            id: key.id,
                            ok: false,
                            remaining: None,
                            limit: None,
                            error: Some(format!("database update failed: {e}")),
                        });
                        continue;
                    }
                    synced += 1;
                    results.push(SyncKeyResult {
                        id: key.id,
                        ok: true,
                        remaining: Some(snap.remaining),
                        limit: Some(snap.limit),
                        error: None,
                    });
                }
                Err(e) => {
                    errors += 1;
                    results.push(SyncKeyResult {
                        id: key.id,
                        ok: false,
                        remaining: None,
                        limit: None,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
    }

    Ok(SyncCreditsReport {
        service: report_service,
        synced,
        errors,
        skipped,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_providers::{ExaClient, FirecrawlClient, TavilyClient, XaiClient};
    use std::sync::Arc;

    #[tokio::test]
    async fn service_list_failure_continues_instead_of_aborting_batch() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        db.insert_api_key("exa", "ek-test")
            .await
            .expect("insert key");
        let providers = ProviderRegistry::from_env();

        // Force every per-service list query to fail; the sync must still
        // return Ok with each failure counted rather than aborting the batch.
        db.pool().close().await;
        let report =
            sync_credits_for_services(&db, &providers, &["exa", "xai"], MAX_KEYS_PER_SERVICE)
                .await
                .expect("per-service failures are reported, not fatal");
        assert_eq!(report.service, "all");
        assert_eq!(report.synced, 0);
        assert_eq!(report.errors, 2);
        assert_eq!(report.results.len(), 2);
        assert!(report.results.iter().all(|r| !r.ok));
        assert!(report.results.iter().all(|r| {
            r.error
                .as_deref()
                .unwrap_or_default()
                .contains("key list failed")
        }));
    }

    #[tokio::test]
    async fn per_key_fetch_errors_are_soft_and_counted() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        db.insert_api_key("exa", "ek-test")
            .await
            .expect("insert key");
        let providers = ProviderRegistry::from_env();

        // exa/xai have no usage endpoint → soft 501 per key, never an abort.
        let report = sync_credits_for_services(&db, &providers, &["exa"], MAX_KEYS_PER_SERVICE)
            .await
            .expect("soft per-key errors do not abort");
        assert_eq!(report.service, "exa");
        assert_eq!(report.synced, 0);
        assert_eq!(report.errors, 1);
        assert_eq!(report.results.len(), 1);
        assert!(!report.results[0].ok);
    }

    // --- F54: update_api_key_usage failure logs the underlying error ---------

    /// Tiny canned HTTP server serving one `GET /usage` success response.
    fn spawn_usage_mock() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let mut read = 0;
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") && read < buf.len() {
                match stream.read(&mut buf[read..]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => read += n,
                }
            }
            let body =
                br#"{"account":{"plan_limit":100,"plan_usage":0},"key":{"limit":0,"usage":0}}"#;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(body);
        });
        format!("http://{addr}")
    }

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

    #[tokio::test]
    async fn update_failure_logs_warning_and_carries_error_detail() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        db.insert_api_key("tavily", "tvly-update-fail")
            .await
            .expect("insert key");
        // Force every UPDATE on api_keys to abort (INSERT/SELECT unaffected), so
        // the vendor fetch succeeds and the DB write deterministically fails.
        sqlx::query(
            "CREATE TRIGGER fail_api_key_updates BEFORE UPDATE ON api_keys \
             BEGIN SELECT RAISE(ABORT, 'forced update failure'); END",
        )
        .execute(db.pool())
        .await
        .expect("create failing trigger");

        let base = spawn_usage_mock();
        let providers = ProviderRegistry::with_clients(
            TavilyClient::new(base),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );

        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false) // CI runners emit ANSI escapes; assertions need plain text
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let report = sync_credits_for_services(&db, &providers, &["tavily"], MAX_KEYS_PER_SERVICE)
            .await
            .expect("update failure is soft, not fatal");
        drop(_guard);

        assert_eq!(report.synced, 0);
        assert_eq!(report.errors, 1);
        assert_eq!(report.results.len(), 1);
        assert!(!report.results[0].ok);
        assert!(
            report.results[0]
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("database update failed"),
            "report must carry the failure: {:?}",
            report.results[0].error
        );
        let text = String::from_utf8_lossy(&sink.0.lock()).into_owned();
        assert!(
            text.contains("update_api_key_usage failed"),
            "warn must fire with a stable message: {text}"
        );
        assert!(
            text.contains("forced update failure"),
            "warn must carry the underlying DB error: {text}"
        );
    }

    // --- B5: throttle (vendor usage-cap) + no-write-on-unreadable-body ------

    /// Mock `/usage` that counts requests and answers every one with the SAME
    /// canned body, so a throttle test can prove how many calls the vendor
    /// actually saw.
    ///
    /// The returned [`MockServer`] is a guard: dropping it flips the stop
    /// flag, and the server thread parks in a poll loop rather than blocking
    /// forever in `accept()`. Without that, every test that used this left a
    /// live thread parked on a socket that is never connected again.
    struct MockServer {
        base_url: String,
        hits: Arc<parking_lot::Mutex<usize>>,
        stop: Arc<parking_lot::Mutex<bool>>,
    }

    impl MockServer {
        fn hits(&self) -> usize {
            *self.hits.lock()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            *self.stop.lock() = true;
        }
    }

    fn spawn_counting_usage_mock(body: &'static [u8]) -> MockServer {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(parking_lot::Mutex::new(0usize));
        let stop = Arc::new(parking_lot::Mutex::new(false));
        let (counter, flag) = (hits.clone(), stop.clone());
        std::thread::spawn(move || {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            // Serve until the guard is dropped: each answered request bumps
            // the shared counter.
            while !*flag.lock() {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        // BSD/macOS inherits O_NONBLOCK from the listener onto
                        // the accepted socket (Linux does not), so without this
                        // the read below returns WouldBlock before the request
                        // even lands and the response is written to nothing.
                        if stream.set_nonblocking(false).is_err() {
                            break;
                        }
                        let mut buf = [0u8; 4096];
                        let mut read = 0;
                        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && read < buf.len() {
                            match stream.read(&mut buf[read..]) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => read += n,
                            }
                        }
                        *counter.lock() += 1;
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        MockServer {
            base_url: format!("http://{addr}"),
            hits,
            stop,
        }
    }

    const RECOGNIZED_TAVILY_BODY: &[u8] =
        br#"{"account":{"plan_limit":100,"plan_usage":40,"paygo_limit":0,"paygo_usage":0}}"#;

    /// The Tavily usage API allows ~10 calls / 10 min. A pool larger than the
    /// cap must not be drained in one pass, and the deferral must be REPORTED
    /// (`skipped`) rather than looking like a complete sync.
    #[tokio::test]
    async fn tavily_pass_is_capped_and_reports_skipped_keys() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        for i in 0..(MAX_KEYS_PER_SERVICE + 5) {
            db.insert_api_key("tavily", &format!("tvly-cap-{i:02}"))
                .await
                .expect("insert key");
        }
        let mock = spawn_counting_usage_mock(RECOGNIZED_TAVILY_BODY);
        let providers = ProviderRegistry::with_clients(
            TavilyClient::new(mock.base_url.clone()),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );

        let report = sync_credits_for_services(&db, &providers, &["tavily"], MAX_KEYS_PER_SERVICE)
            .await
            .expect("sync");

        assert_eq!(report.synced as usize, MAX_KEYS_PER_SERVICE);
        assert_eq!(report.skipped, 5, "the over-cap keys must be reported");
        assert_eq!(report.results.len(), MAX_KEYS_PER_SERVICE);
        assert_eq!(
            mock.hits(),
            MAX_KEYS_PER_SERVICE,
            "vendor must never see more than the cap in one pass"
        );
        // The capped-away rows are untouched, not half-written.
        let unwritten: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE credits_remaining IS NULL")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(unwritten, 5);
    }

    /// A 200 whose body carries no credit field is not "0 credits left" — the
    /// stored snapshot must be left exactly as it was.
    #[tokio::test]
    async fn unrecognized_usage_body_writes_nothing() {
        let db = serpotter_db::connect_and_migrate("sqlite::memory:")
            .await
            .expect("in-memory db");
        let key = db
            .insert_api_key("tavily", "tvly-unreadable")
            .await
            .expect("insert key");
        db.set_api_key_credits(key.id, Some(77))
            .await
            .expect("seed a known-good snapshot");

        let mock = spawn_counting_usage_mock(br#"{"status":"ok","data":{}}"#);
        let providers = ProviderRegistry::with_clients(
            TavilyClient::new(mock.base_url.clone()),
            FirecrawlClient::new("http://127.0.0.1:9"),
            ExaClient::new("http://127.0.0.1:9"),
            XaiClient::new("http://127.0.0.1:9"),
        );

        let report = sync_credits_for_services(&db, &providers, &["tavily"], MAX_KEYS_PER_SERVICE)
            .await
            .expect("unreadable body is a soft error, not a fatal");
        assert_eq!(mock.hits(), 1, "the key was contacted exactly once");
        assert_eq!(report.synced, 0, "an unreadable body is never a sync");
        assert_eq!(report.errors, 1);

        let row = db.get_api_key_admin(key.id).await.unwrap().unwrap();
        assert_eq!(
            row.credits_remaining,
            Some(77),
            "the last known snapshot must survive an unreadable body"
        );
        assert!(
            row.usage_synced_at.is_none(),
            "a refused parse must not stamp usage_synced_at"
        );
        assert_eq!(row.active, 1, "an unreadable body never deactivates a key");
    }

    #[test]
    fn skipped_count_is_never_negative() {
        assert_eq!(skipped_count(3, 10), 0, "under the cap: nothing skipped");
        assert_eq!(skipped_count(10, 10), 0, "exactly at the cap");
        assert_eq!(skipped_count(25, 10), 15);
    }
}
