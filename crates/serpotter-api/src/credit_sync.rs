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
        let keys = db.list_active_keys_for_service(service).await?;
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
                    if let Err(_e) = db
                        .update_api_key_usage(key.id, snap.remaining, snap.limit)
                        .await
                    {
                        errors += 1;
                        results.push(SyncKeyResult {
                            id: key.id,
                            ok: false,
                            remaining: None,
                            limit: None,
                        });
                        continue;
                    }
                    synced += 1;
                    results.push(SyncKeyResult {
                        id: key.id,
                        ok: true,
                        remaining: Some(snap.remaining),
                        limit: Some(snap.limit),
                    });
                }
                Err(_) => {
                    errors += 1;
                    results.push(SyncKeyResult {
                        id: key.id,
                        ok: false,
                        remaining: None,
                        limit: None,
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
