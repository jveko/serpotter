use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serpotter_api::{app, AppState};
use serpotter_auth::generate_token;
use serpotter_keypool::KeyPool;
use serpotter_outbound::ProxyPool;
use serpotter_providers::ProviderRegistry;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;
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
            let port: u16 = env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080);
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
            // Twin-pool outbound: Fixed env (process-stable) or live nodes/direct.
            // Per-call proxy is resolved via ProductCtx.outbound; providers stay direct-default.
            // REQUIRE_OUTBOUND_PROXY=1|true → product fails closed (NoHealthyNode) when no lease.
            let env_proxy = env::var("OUTBOUND_PROXY")
                .or_else(|_| env::var("HTTPS_PROXY"))
                .or_else(|_| env::var("HTTP_PROXY"))
                .ok();
            let require_proxy = matches!(
                env::var("REQUIRE_OUTBOUND_PROXY")
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str(),
                "1" | "true" | "yes"
            );
            let outbound = Arc::new(ProxyPool::with_options(
                env_proxy,
                db.clone(),
                require_proxy,
            ));
            let providers = ProviderRegistry::from_env();
            tracing::info!(
                require_proxy,
                "providers dial via per-request ProxyPool (xAI always direct)"
            );
            let maint = serpotter_api::cron::spawn_maintenance(db.clone(), providers.clone());
            let router = app(AppState {
                db,
                keys,
                outbound,
                providers,
                admin_secret,
            })
            .layer(TraceLayer::new_for_http())
            .layer(PropagateRequestIdLayer::x_request_id())
            .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("bind {addr}"))?;
            tracing::info!(%addr, "listening");
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown_signal())
                .await
                .context("serve")?;
            maint.abort();
            let _ = maint.await;
            Ok(())
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("ctrl_c handler");
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

fn parse_seed_key(
    args: &mut impl Iterator<Item = String>,
) -> anyhow::Result<(String, String)> {
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
