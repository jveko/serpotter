//! Lean in-process key pool over sqlx `api_keys`.
//!
//! Single-process mutex serializes acquire; durable state lives in SQLite.

use serpotter_db::{ApiKeyRow, Db, DbError};
use thiserror::Error;
use tokio::sync::Mutex;

pub const MAX_BATCH: usize = 10;

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
        let mut batch = self.acquire_batch(service, 1).await?;
        batch
            .pop()
            .ok_or_else(|| KeyPoolError::NoHealthyKey(service.to_string()))
    }

    /// Acquire up to `n` distinct keys (`n` clamped to 1..=10). Empty vec → NoHealthyKey.
    pub async fn acquire_batch(
        &self,
        service: &str,
        n: usize,
    ) -> Result<Vec<LeasedKey>, KeyPoolError> {
        let n = n.clamp(1, MAX_BATCH);
        let _guard = self.lock.lock().await;
        let rows = self.db.acquire_api_keys_batch(service, n).await?;
        if rows.is_empty() {
            return Err(KeyPoolError::NoHealthyKey(service.to_string()));
        }
        Ok(rows.into_iter().map(to_lease).collect())
    }

    pub async fn report_success(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_success(id).await?;
        Ok(())
    }

    pub async fn report_failure(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_failure(id).await?;
        Ok(())
    }

    pub async fn report_exhausted(&self, id: i64) -> Result<(), KeyPoolError> {
        self.db.report_api_key_exhausted(id).await?;
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

    #[tokio::test]
    async fn acquire_batch_distinct_keys() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        db.insert_api_key("tavily", "tvly-a").await.unwrap();
        db.insert_api_key("tavily", "tvly-b").await.unwrap();
        db.insert_api_key("tavily", "tvly-c").await.unwrap();
        let pool = KeyPool::new(db);
        let batch = pool.acquire_batch("tavily", 10).await.unwrap();
        assert_eq!(batch.len(), 3);
        let mut keys: Vec<_> = batch.iter().map(|k| k.key.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["tvly-a", "tvly-b", "tvly-c"]);
        let ids: std::collections::HashSet<_> = batch.iter().map(|k| k.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[tokio::test]
    async fn acquire_batch_empty_is_no_healthy() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let pool = KeyPool::new(db);
        let err = pool.acquire_batch("tavily", 3).await.unwrap_err();
        assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
    }

    #[tokio::test]
    async fn report_exhausted_prefers_other_key() {
        let db = connect_and_migrate("sqlite::memory:").await.unwrap();
        let a = db.insert_api_key("tavily", "tvly-a").await.unwrap();
        let b = db.insert_api_key("tavily", "tvly-b").await.unwrap();
        db.set_api_key_credits(a.id, Some(10)).await.unwrap();
        db.set_api_key_credits(b.id, Some(10)).await.unwrap();
        let pool = KeyPool::new(db);
        pool.report_exhausted(a.id).await.unwrap();
        // First pick: b (priority 1). Stamps b more recent than exhausted a.
        let first = pool.acquire("tavily").await.unwrap();
        assert_eq!(first.id, b.id);
        // Soft lease blocks re-acquire until report clears it.
        pool.report_success(first.id).await.unwrap();
        // Pure LRU would now prefer older a; CASE must still prefer healthy b.
        let second = pool.acquire("tavily").await.unwrap();
        assert_eq!(
            second.id, b.id,
            "credit priority must beat LRU favoring exhausted key"
        );
    }
}
