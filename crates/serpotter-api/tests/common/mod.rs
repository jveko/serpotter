use std::sync::Arc;

use http_body_util::BodyExt;
use serde_json::Value;
use serpotter_api::{AppState, McpSessionStore};
use serpotter_db::connect_and_migrate;
use serpotter_keypool::KeyPool;
use serpotter_providers::{
    ExaClient, FirecrawlClient, ProviderRegistry, TavilyClient, XaiClient,
};

/// Fixed API token used across integration suites (valid length/shape).
pub const TEST_TOKEN: &str = "tok-validtokenfortest0000000000000000";

/// Admin secret wired into [`state_with`].
pub const TEST_ADMIN_SECRET: &str = "test-admin-secret";

pub async fn test_db() -> serpotter_db::Db {
    connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate")
}

/// App state with providers pointed at `127.0.0.1:9` (connection refused, no network).
pub fn state_with(db: serpotter_db::Db) -> AppState {
    AppState {
        keys: Arc::new(KeyPool::new(db.clone())),
        providers: ProviderRegistry {
            tavily: TavilyClient::new("http://127.0.0.1:9"),
            firecrawl: FirecrawlClient::new("http://127.0.0.1:9"),
            exa: ExaClient::new("http://127.0.0.1:9"),
            xai: XaiClient::new("http://127.0.0.1:9"),
        },
        db,
        admin_secret: Some(TEST_ADMIN_SECRET.into()),
        mcp_sessions: McpSessionStore::new(),
    }
}

pub async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

pub use axum::body::Body;
pub use axum::http::{Request, StatusCode};
pub use serpotter_api::app;
pub use tower::ServiceExt;
