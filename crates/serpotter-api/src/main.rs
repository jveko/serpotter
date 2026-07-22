use std::env;
use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use serpotter_api::{app, AppState};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,serpotter_api=debug"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let database_url = env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite:data/serpotter.db?mode=rwc".to_string());
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    let environment = env::var("ENVIRONMENT").unwrap_or_else(|_| "development".to_string());

    if let Some(path) = sqlite_file_path(&database_url) {
        if let Some(parent) = Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create data dir {}", parent.display()))?;
            }
        }
    }

    tracing::info!(%environment, %port, "starting serpotter-api");

    let db = serpotter_db::connect_and_migrate(&database_url)
        .await
        .context("database connect/migrate")?;
    let router = app(AppState { db }).layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    tracing::info!(%addr, "listening");
    axum::serve(listener, router).await.context("serve")?;
    Ok(())
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
