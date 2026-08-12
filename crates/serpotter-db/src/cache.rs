use crate::{Db, DbError};
use sqlx::Row;

/// One `query_cache` row (B1 exact-query TTL cache).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheRow {
    pub service: String,
    pub key_hash: String,
    pub response_json: String,
    pub created_at: String,
    pub expires_at: String,
}

impl Db {
    /// Fetch a not-yet-expired cached response for `(service, key_hash)`.
    ///
    /// Expiry is checked in SQL (`expires_at > datetime('now')`) so a caller
    /// that races the purge still never sees a stale entry.
    pub async fn cache_get(
        &self,
        service: &str,
        key_hash: &str,
    ) -> Result<Option<String>, DbError> {
        let row = sqlx::query(
            "SELECT response_json FROM query_cache \
             WHERE key_hash = ? AND service = ? AND expires_at > datetime('now')",
        )
        .bind(key_hash)
        .bind(service)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(r.try_get("response_json")?),
            None => None,
        })
    }

    /// Insert or refresh a cache entry with `ttl_secs` lifetime.
    ///
    /// Refreshing an existing `key_hash` bumps `created_at` and re-extends
    /// `expires_at` (the key is the service-aware hash, so a collision across
    /// services implies the caller's hash is not service-aware — documented in
    /// the DDL contract, key_hash is PRIMARY KEY).
    pub async fn cache_put(
        &self,
        service: &str,
        key_hash: &str,
        response_json: &str,
        ttl_secs: i64,
    ) -> Result<(), DbError> {
        let ttl = ttl_secs.max(1);
        sqlx::query(
            "INSERT INTO query_cache (service, key_hash, response_json, created_at, expires_at) \
             VALUES (?, ?, ?, datetime('now'), datetime('now', '+' || ? || ' seconds')) \
             ON CONFLICT(key_hash) DO UPDATE SET \
               service = excluded.service, \
               response_json = excluded.response_json, \
               created_at = datetime('now'), \
               expires_at = excluded.expires_at",
        )
        .bind(service)
        .bind(key_hash)
        .bind(response_json)
        .bind(ttl)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Delete expired entries (maintenance tick), returning rows affected.
    pub async fn purge_expired_cache(&self) -> Result<u64, DbError> {
        let r = sqlx::query("DELETE FROM query_cache WHERE expires_at < datetime('now')")
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        Db::connect_for_test().await
    }

    #[tokio::test]
    async fn cache_put_get_roundtrip() {
        let db = db().await;
        db.cache_put("tavily", "h1", r#"{"ok":true}"#, 300)
            .await
            .unwrap();
        let got = db.cache_get("tavily", "h1").await.unwrap();
        assert_eq!(got.as_deref(), Some(r#"{"ok":true}"#));
    }

    #[tokio::test]
    async fn cache_get_respects_service() {
        let db = db().await;
        db.cache_put("tavily", "h1", "a", 300).await.unwrap();
        // Different service, same hash → no hit (caller hashes per service).
        assert_eq!(db.cache_get("firecrawl", "h1").await.unwrap(), None);
        assert_eq!(
            db.cache_get("tavily", "h1").await.unwrap().as_deref(),
            Some("a")
        );
    }

    #[tokio::test]
    async fn cache_get_misses_when_expired() {
        let db = db().await;
        db.cache_put("tavily", "h1", "old", 300).await.unwrap();
        // Force expiry (and created_at) into the past.
        sqlx::query("UPDATE query_cache SET expires_at = datetime('now', '-1 second') WHERE key_hash = 'h1'")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(db.cache_get("tavily", "h1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn cache_put_refresh_overwrites_and_extends() {
        let db = db().await;
        db.cache_put("tavily", "h1", "v1", 300).await.unwrap();
        db.cache_put("tavily", "h1", "v2", 300).await.unwrap();
        let got = db.cache_get("tavily", "h1").await.unwrap();
        assert_eq!(
            got.as_deref(),
            Some("v2"),
            "refresh must replace the payload"
        );
        let row = sqlx::query(
            "SELECT service, key_hash, response_json, created_at, expires_at FROM query_cache WHERE key_hash = 'h1'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let created_at: String = row.try_get("created_at").unwrap();
        let expires_at: String = row.try_get("expires_at").unwrap();
        assert!(expires_at > created_at, "refresh must re-extend expiry");
    }

    #[tokio::test]
    async fn purge_expired_cache_deletes_only_expired() {
        let db = db().await;
        db.cache_put("tavily", "fresh", "x", 300).await.unwrap();
        db.cache_put("tavily", "stale", "y", 300).await.unwrap();
        sqlx::query("UPDATE query_cache SET expires_at = datetime('now', '-1 second') WHERE key_hash = 'stale'")
            .execute(db.pool())
            .await
            .unwrap();
        let purged = db.purge_expired_cache().await.unwrap();
        assert_eq!(purged, 1);
        assert!(db.cache_get("tavily", "stale").await.unwrap().is_none());
        assert!(db.cache_get("tavily", "fresh").await.unwrap().is_some());
    }
}
