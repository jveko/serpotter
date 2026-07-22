//! Lean in-process key pool over sqlx `api_keys`.
//!
//! Single-process mutex serializes acquire; durable state lives in SQLite.

use serpotter_db::{ApiKeyRow, Db, DbError};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Debug, Error)]
pub enum KeyPoolError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("no healthy key for service {0}")]
    NoHealthyKey(String),
}

#[derive(Clone, Debug)]
pub struct LeasedKey {
    pub id: i64,
    pub service: String,
    pub key: String,
}

pub struct KeyPool {
    db: Db,
    /// Serializes acquire so two concurrent searches don't stampede the same LRU pick
    /// without last_used_at updates landing first.
    lock: Mutex<()>,
}

impl KeyPool {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            lock: Mutex::new(()),
        }
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    pub async fn acquire(&self, service: &str) -> Result<LeasedKey, KeyPoolError> {
        let _guard = self.lock.lock().await;
        match self.db.acquire_api_key(service).await? {
            Some(row) => Ok(to_lease(row)),
            None => Err(KeyPoolError::NoHealthyKey(service.to_string())),
        }
    }

    pub async fn report_success(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_success(id).await?;
        Ok(())
    }

    pub async fn report_failure(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_failure(id).await?;
        Ok(())
    }
}

fn to_lease(row: ApiKeyRow) -> LeasedKey {
    LeasedKey {
        id: row.id,
        service: row.service,
        key: row.key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serpotter_db::connect_and_migrate;

    #[tokio::test]
    async fn acquire_none_is_no_healthy() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let pool = KeyPool::new(db);
        let err = pool.acquire("tavily").await.unwrap_err();
        assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
    }

    #[tokio::test]
    async fn acquire_then_success() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        db.insert_api_key("tavily", "tvly-x").await.unwrap();
        let pool = KeyPool::new(db);
        let lease = pool.acquire("tavily").await.unwrap();
        assert_eq!(lease.key, "tvly-x");
        pool.report_success(lease.id).await.unwrap();
    }
}
