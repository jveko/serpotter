//! Soft-fail credit sync for provider keys (admin + optional cron).
//! Tavily/Firecrawl: real usage endpoints. Exa/xAI: no reliable public usage API —
//! counting as soft errors only (never write fake credits, never deactivate).

use serpotter_db::Db;
use serpotter_providers::ProviderRegistry;

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
    pub results: Vec<SyncKeyResult>,
}

/// Sync active keys for `services` (`tavily`/`firecrawl` real usage; `exa`/`xai` soft-error only).
/// Soft-fail per key: never sets active=0 on fetch/DB error.
pub async fn sync_credits_for_services(
    db: &Db,
    providers: &ProviderRegistry,
    services: &[&str],
) -> Result<SyncCreditsReport, serpotter_db::DbError> {
    let report_service = if services.len() == 1 {
        services[0].to_string()
    } else {
        "all".to_string()
    };

    let mut synced: i64 = 0;
    let mut errors: i64 = 0;
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
        for key in keys {
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
        let report = sync_credits_for_services(&db, &providers, &["exa", "xai"])
            .await
            .expect("per-service failures are reported, not fatal");
        assert_eq!(report.service, "all");
        assert_eq!(report.synced, 0);
        assert_eq!(report.errors, 2);
        assert_eq!(report.results.len(), 2);
        assert!(report.results.iter().all(|r| !r.ok));
        assert!(report.results.iter().all(|r| r
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("key list failed")));
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
        let report = sync_credits_for_services(&db, &providers, &["exa"])
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
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let report = sync_credits_for_services(&db, &providers, &["tavily"])
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
}
