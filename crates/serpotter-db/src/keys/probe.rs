use super::rows::ApiKeyRow;
use crate::{Db, DbError};
use sqlx::Row;

impl Db {
    /// The ONLY eligibility gate for the daily key health probe: active,
    /// not in a live cooldown, and not already stamped today. The
    /// once-per-day guarantee lives entirely in this SQL predicate — callers
    /// (the probe loop) must not re-implement any part of it in Rust.
    pub async fn due_probe_keys(&self) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails,
       COALESCE(key_fingerprint, '') AS key_fingerprint, last_probe_at
  FROM api_keys
 WHERE active = 1
   AND (cooldown_until IS NULL OR cooldown_until <= datetime('now'))
   AND (last_probe_at IS NULL OR last_probe_at < date('now'))
 ORDER BY id ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(ApiKeyRow {
                    id: r.try_get("id")?,
                    service: r.try_get("service")?,
                    key: r.try_get("key")?,
                    active: r.try_get("active")?,
                    consecutive_fails: r.try_get("consecutive_fails")?,
                    key_fingerprint: r.try_get("key_fingerprint")?,
                    last_probe_at: r.try_get("last_probe_at")?,
                })
            })
            .collect()
    }

    /// Stamp `id` as probed today using the database server's clock
    /// (`date('now')`), never the caller's wall clock, so the stamp always
    /// compares against the same clock the due query's `date('now')` uses.
    pub async fn stamp_key_probe(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET last_probe_at = date('now') WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        crate::connect_and_migrate("sqlite::memory:")
            .await
            .expect("migrate")
    }

    async fn due_ids(db: &Db) -> Vec<i64> {
        db.due_probe_keys()
            .await
            .expect("due_probe_keys")
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    #[tokio::test]
    async fn due_null_stamp_is_due() {
        let db = db().await;
        let row = db
            .insert_api_key("tavily", "tvly-probe-null")
            .await
            .unwrap();
        assert_eq!(due_ids(&db).await, vec![row.id]);
    }

    #[tokio::test]
    async fn due_skips_row_stamped_today() {
        let db = db().await;
        let row = db
            .insert_api_key("tavily", "tvly-probe-stamped")
            .await
            .unwrap();
        db.stamp_key_probe(row.id).await.unwrap();
        assert_eq!(due_ids(&db).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn due_yesterday_stamp_is_due() {
        let db = db().await;
        let row = db
            .insert_api_key("tavily", "tvly-probe-yesterday")
            .await
            .unwrap();
        sqlx::query("UPDATE api_keys SET last_probe_at = date('now','-1 day') WHERE id = ?")
            .bind(row.id)
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(due_ids(&db).await, vec![row.id]);
    }

    #[tokio::test]
    async fn due_inactive_never_due() {
        let db = db().await;
        let row = db
            .insert_api_key("tavily", "tvly-probe-inactive")
            .await
            .unwrap();
        db.set_api_key_active(row.id, false).await.unwrap();
        assert_eq!(due_ids(&db).await, Vec::<i64>::new());
    }

    /// A live cooldown is a vendor rate-limit window: probing B during it
    /// would burn quota. An expired cooldown (C) is not a barrier.
    #[tokio::test]
    async fn due_skips_inactive_and_live_cooldown_keys() {
        let db = db().await;
        let a = db.insert_api_key("tavily", "tvly-probe-a").await.unwrap();
        let b = db.insert_api_key("tavily", "tvly-probe-b").await.unwrap();
        let c = db.insert_api_key("tavily", "tvly-probe-c").await.unwrap();
        sqlx::query("UPDATE api_keys SET cooldown_until = datetime('now','+1 hour') WHERE id = ?")
            .bind(b.id)
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE api_keys SET cooldown_until = datetime('now','-1 hour') WHERE id = ?")
            .bind(c.id)
            .execute(db.pool())
            .await
            .unwrap();

        let due = due_ids(&db).await;
        assert!(due.contains(&a.id), "A (no cooldown) must be due: {due:?}");
        assert!(
            due.contains(&c.id),
            "C (expired cooldown) must be due: {due:?}"
        );
        assert!(
            !due.contains(&b.id),
            "B (live cooldown) must be skipped: {due:?}"
        );

        // Stamp is per-row: stamp everything currently due, nothing is due.
        for id in &due {
            db.stamp_key_probe(*id).await.unwrap();
        }
        assert_eq!(due_ids(&db).await, Vec::<i64>::new());
    }

    #[tokio::test]
    async fn stamp_writes_server_date() {
        let db = db().await;
        let row = db
            .insert_api_key("tavily", "tvly-probe-server-date")
            .await
            .unwrap();
        db.stamp_key_probe(row.id).await.unwrap();

        let stamped: Option<String> =
            sqlx::query("SELECT last_probe_at FROM api_keys WHERE id = ?")
                .bind(row.id)
                .fetch_one(db.pool())
                .await
                .unwrap()
                .try_get("last_probe_at")
                .unwrap();
        let today: String = sqlx::query("SELECT date('now') AS today")
            .fetch_one(db.pool())
            .await
            .unwrap()
            .try_get("today")
            .unwrap();
        assert_eq!(stamped.as_deref(), Some(today.as_str()));
    }
}
