use std::env;
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use serpotter_api::{app, AppState};
use serpotter_auth::generate_token;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::ProviderRegistry;
use serpotter_providers::PROVIDER_SERVICES;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,serpotter_api=debug"));
    let json_logs = env::var("LOG_FORMAT")
        .map(|v| v.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if json_logs {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    let mut args = env::args().skip(1);
    let cmd = args.next();

    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:data/serpotter.db?mode=rwc".to_string());

    if let Some(path) = sqlite_file_path(&database_url) {
        if let Some(parent) = Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create data dir {}", parent.display()))?;
            }
        }
    }

    let db = serpotter_db::connect_and_migrate(&database_url)
        .await
        .context("database connect/migrate")?;

    match cmd.as_deref() {
        Some("seed-token") => {
            let name = parse_name_flag(&mut args);
            let token = generate_token().map_err(|e| anyhow::anyhow!("generate token: {e}"))?;
            let row = db
                .insert_token(&token, &name)
                .await
                .context("insert token")?;
            eprintln!("id={} name={:?}", row.id, row.name);
            println!("{}", row.token);
            Ok(())
        }
        Some("seed-key") => {
            let (service, key) = parse_seed_key(&mut args)?;
            if !PROVIDER_SERVICES.contains(&service.as_str()) {
                anyhow::bail!(
                    "unsupported service {service:?}; expected one of {}",
                    PROVIDER_SERVICES.join(", ")
                );
            }
            let row = db
                .insert_api_key(&service, &key)
                .await
                .context("insert api key")?;
            eprintln!(
                "id={} service={} active={}",
                row.id, row.service, row.active
            );
            Ok(())
        }
        Some(other) => {
            anyhow::bail!("unknown command {other:?}; use seed-token | seed-key | (none to serve)")
        }
        None => {
            let port = port_from_env();
            let environment = env::var("ENVIRONMENT").unwrap_or_else(|_| "development".to_string());
            tracing::info!(%environment, %port, "starting serpotter-api");

            let admin_secret = env::var("ADMIN_SECRET").ok().filter(|s| !s.is_empty());
            // Process-start hygiene: drop orphan key/node holds from a previous crash.
            if let Err(e) = db.zero_all_key_inflight().await {
                tracing::warn!(error = %e, "zero_all_key_inflight failed");
            }
            if let Err(e) = db.zero_all_node_inflight().await {
                tracing::warn!(error = %e, "zero_all_node_inflight failed");
            }
            let keys = Arc::new(KeyPool::new(db.clone()));
            // Nodes-only outbound; product resolves per-call via ProductCtx.outbound.
            // REQUIRE_OUTBOUND_PROXY=1|true → product fails closed (NoHealthyNode) when no lease.
            // xAI always dials direct; OUTBOUND_PROXY/HTTPS_PROXY/HTTP_PROXY env ignored.
            let require_proxy = matches!(
                env::var("REQUIRE_OUTBOUND_PROXY")
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "1" | "true" | "yes"
            );
            let outbound = Arc::new(ProxyPool::with_options(db.clone(), require_proxy));
            let providers = ProviderRegistry::from_env();
            tracing::info!(
                require_proxy,
                "outbound ProxyPool is nodes-only (xAI always direct; OUTBOUND_PROXY env ignored)"
            );
            let events = serpotter_api::events::RequestEvents::new();
            let maint = serpotter_api::cron::spawn_maintenance(
                db.clone(),
                providers.clone(),
                events.clone(),
            );
            // The full request-id + trace + body-limit stack is assembled
            // inside `app` (lib.rs `app_with_spa`) so the production router
            // and the integration-test router share one identical stack; no
            // layers are added here.
            let router = app(AppState {
                db,
                keys,
                outbound,
                providers,
                admin_secret,
                events,
            });
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("bind {addr}"))?;
            tracing::info!(%addr, "listening");

            // Two-stage graceful drain: the serve future is polled from the
            // start, and the ~20s drain cap is armed ONLY after the shutdown
            // signal fires (arming at startup would self-terminate a
            // long-running server). On cap expiry we warn and drop the serve
            // future so long-lived MCP SSE streams cannot stall shutdown.
            const DRAIN_GRACE_SECS: u64 = 20;
            let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
            let serve_rx = shutdown_rx.clone();
            let signal_task = tokio::spawn(async move {
                shutdown_signal().await;
                let _ = shutdown_tx.send(true);
            });
            let server = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let mut rx = serve_rx;
                    let _ = rx.wait_for(|fired| *fired).await;
                })
                .into_future();
            tokio::pin!(server);

            let mut drain_armed = false;
            loop {
                tokio::select! {
                    biased;
                    // Serve finished (signal fired and in-flight drained).
                    result = &mut server => {
                        result.context("serve")?;
                        break;
                    }
                    // Signal fired → arm the drain cap for this shutdown.
                    _ = shutdown_rx.changed(), if !drain_armed => {
                        drain_armed = true;
                        tracing::info!(
                            cap_secs = DRAIN_GRACE_SECS,
                            "shutdown signal received; draining in-flight requests"
                        );
                    }
                    // Cap expired: drop the serve future, ending SSE streams.
                    _ = tokio::time::sleep(Duration::from_secs(DRAIN_GRACE_SECS)),
                        if drain_armed =>
                    {
                        tracing::warn!(
                            cap_secs = DRAIN_GRACE_SECS,
                            "drain cap expired; forcing shutdown"
                        );
                        break;
                    }
                }
            }
            let _ = signal_task.await;
            maint.abort();
            let _ = maint.await;
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.expect("ctrl_c handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("sigterm handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

fn parse_name_flag(args: &mut impl Iterator<Item = String>) -> String {
    match args.next().as_deref() {
        Some("--name") => args.next().unwrap_or_default(),
        Some(other) if !other.starts_with('-') => other.to_string(),
        _ => String::new(),
    }
}

fn parse_seed_key(args: &mut impl Iterator<Item = String>) -> anyhow::Result<(String, String)> {
    let mut service = "tavily".to_string();
    let mut key = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--service" => {
                service = args.next().context("--service requires a value")?;
            }
            "--key" => {
                key = Some(args.next().context("--key requires a value")?);
            }
            other if !other.starts_with('-') && key.is_none() => {
                key = Some(other.to_string());
            }
            other => anyhow::bail!("unexpected seed-key arg {other}"),
        }
    }
    let key = key.context("seed-key requires --key <API_KEY>")?;
    Ok((service, key))
}

fn sqlite_file_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("sqlite:")?;
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let path = rest.split('?').next()?.to_string();
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    Some(path)
}

/// Read `PORT` (default 8080). A set-but-unparseable value is warned about
/// (never silently ignored) and falls back to the default — an operator typo
/// like `PORT=8443x` must be visible in startup logs instead of silently
/// binding a different port.
fn port_from_env() -> u16 {
    match env::var("PORT") {
        Ok(raw) => match raw.parse::<u16>() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(
                    raw_value = %raw,
                    default = 8080,
                    "PORT is not a valid port (0-65535); binding default 8080"
                );
                8080
            }
        },
        Err(_) => 8080,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes process-env mutation so parallel tests never race set/remove.
    static ENV_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

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
        let sink = CaptureSink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let guard = sink.0.lock();
        String::from_utf8_lossy(&guard).into_owned()
    }

    #[test]
    fn invalid_port_warns_and_binds_default() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("PORT", "8443x");
        let text = capture_warns(|| {
            assert_eq!(port_from_env(), 8080);
        });
        std::env::remove_var("PORT");
        assert!(text.contains("PORT"), "warn must name the var: {text}");
        assert!(
            text.contains("8443x"),
            "warn must carry the raw offending value: {text}"
        );
    }

    #[test]
    fn parseable_port_wins_and_unset_defaults() {
        let _guard = ENV_LOCK.lock();
        std::env::set_var("PORT", "9000");
        assert_eq!(port_from_env(), 9000);
        std::env::remove_var("PORT");
        assert_eq!(port_from_env(), 8080);
    }
}
