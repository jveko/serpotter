use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use serpotter_api::{app, AppState};
use serpotter_auth::generate_token;
use serpotter_keypool::KeyPool;
use serpotter_providers::ProviderRegistry;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,serpotter_api=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

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

            let keys = Arc::new(KeyPool::new(db.clone()));
            let providers = ProviderRegistry::from_env();
            let router = app(AppState {
                db,
                keys,
                providers,
            })
            .layer(TraceLayer::new_for_http());
            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = tokio::net::TcpListener::bind(addr)
                .await
                .with_context(|| format!("bind {addr}"))?;
            tracing::info!(%addr, "listening");
            axum::serve(listener, router).await.context("serve")?;
            Ok(())
        }
    }
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
