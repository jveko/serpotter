use crate::{Db, DbError};
use sqlx::Row;

/// One `provider_jobs` row (B16 async job abstraction).
/// `status` is 'running' | 'done' | 'failed'; `result_json`/`error` carry the
/// terminal payload (result_json for 'done', error message for 'failed').
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderJobRow {
    pub id: String,
    pub kind: String,
    pub service: String,
    pub params_json: String,
    pub status: String,
    pub result_json: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: String,
}

impl Db {
    /// Create a job row with status 'running' and a `ttl_secs` expiry
    /// (`expires_at = datetime('now', '+' || ttl || ' seconds')`). The id is
    /// caller-minted (API layer).
    pub async fn create_job(
        &self,
        id: &str,
        kind: &str,
        service: &str,
        params_json: &str,
        ttl_secs: i64,
    ) -> Result<ProviderJobRow, DbError> {
        let ttl = ttl_secs.max(1);
        let r = sqlx::query(
            "INSERT INTO provider_jobs (id, kind, service, params_json, status, expires_at) \
             VALUES (?, ?, ?, ?, 'running', datetime('now', '+' || ? || ' seconds')) \
             RETURNING id, kind, service, params_json, status, result_json, error, \
                       created_at, updated_at, expires_at",
        )
        .bind(id)
        .bind(kind)
        .bind(service)
        .bind(params_json)
        .bind(ttl)
        .fetch_one(&self.pool)
        .await?;
        map_job_row(&r)
    }

    /// Set a job's terminal (or progress) state. Returns `true` iff a row with
    /// `id` existed (false = unknown id, lets the API return 404).
    pub async fn update_job_result(
        &self,
        id: &str,
        status: &str,
        result_json: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool, DbError> {
        let r = sqlx::query(
            "UPDATE provider_jobs SET \
               status = ?, \
               result_json = ?, \
               error = ?, \
               updated_at = datetime('now') \
             WHERE id = ?",
        )
        .bind(status)
        .bind(result_json)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Fetch one job by id.
    pub async fn get_job(&self, id: &str) -> Result<Option<ProviderJobRow>, DbError> {
        let row = sqlx::query(
            "SELECT id, kind, service, params_json, status, result_json, error, \
                    created_at, updated_at, expires_at \
             FROM provider_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(match row {
            Some(r) => Some(map_job_row(&r)?),
            None => None,
        })
    }

    /// Newest-first job page (created_at tie broken by id DESC, matching the
    /// request_log convention). `limit` clamped 1..=100.
    pub async fn list_jobs(&self, limit: i64) -> Result<Vec<ProviderJobRow>, DbError> {
        let limit = limit.clamp(1, 100);
        let rows = sqlx::query(
            "SELECT id, kind, service, params_json, status, result_json, error, \
                    created_at, updated_at, expires_at \
             FROM provider_jobs \
             ORDER BY created_at DESC, id DESC \
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(map_job_row(&r)?);
        }
        Ok(out)
    }

    /// Delete jobs past their expiry (maintenance tick), returning rows
    /// affected. A running job past its TTL is also removed — the runner must
    /// refresh its TTL by re-creating/updating if it wants to outlive it.
    pub async fn purge_expired_jobs(&self) -> Result<u64, DbError> {
        let r = sqlx::query("DELETE FROM provider_jobs WHERE expires_at < datetime('now')")
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }
}

fn map_job_row(r: &sqlx::sqlite::SqliteRow) -> Result<ProviderJobRow, DbError> {
    Ok(ProviderJobRow {
        id: r.try_get("id")?,
        kind: r.try_get("kind")?,
        service: r.try_get("service")?,
        params_json: r.try_get("params_json")?,
        status: r.try_get("status")?,
        result_json: r.try_get("result_json")?,
        error: r.try_get("error")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        Db::connect_for_test().await
    }

    #[tokio::test]
    async fn create_get_job_lifecycle() {
        let db = db().await;
        let job = db
            .create_job("job-1", "tavily_research", "tavily", r#"{"q":"x"}"#, 3600)
            .await
            .unwrap();
        assert_eq!(job.id, "job-1");
        assert_eq!(job.status, "running");
        assert_eq!(job.kind, "tavily_research");
        assert!(job.result_json.is_none());
        assert!(job.error.is_none());
        assert!(
            job.expires_at > job.created_at,
            "expiry must be in the future"
        );

        let fetched = db.get_job("job-1").await.unwrap().expect("job exists");
        assert_eq!(fetched, job);
        assert_eq!(db.get_job("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn update_job_result_sets_terminal_state_and_bumps_updated_at() {
        let db = db().await;
        db.create_job("job-2", "kind", "tavily", "{}", 3600)
            .await
            .unwrap();
        let updated = db
            .update_job_result("job-2", "done", Some(r#"{"answer":"ok"}"#), None)
            .await
            .unwrap();
        assert!(updated, "existing job updates");
        let job = db.get_job("job-2").await.unwrap().unwrap();
        assert_eq!(job.status, "done");
        assert_eq!(job.result_json.as_deref(), Some(r#"{"answer":"ok"}"#));
        assert!(job.error.is_none());
        assert!(job.updated_at >= job.created_at);

        // Failed path.
        db.update_job_result("job-2", "failed", None, Some("boom"))
            .await
            .unwrap();
        let job = db.get_job("job-2").await.unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert_eq!(job.error.as_deref(), Some("boom"));

        // Unknown id → false.
        let missing = db
            .update_job_result("nope", "done", None, None)
            .await
            .unwrap();
        assert!(!missing);
    }

    #[tokio::test]
    async fn list_jobs_newest_first_respects_limit() {
        let db = db().await;
        for i in 0..5 {
            db.create_job(&format!("job-{i}"), "kind", "tavily", "{}", 3600)
                .await
                .unwrap();
            // Space created_at so ordering is deterministic (sub-second ties
            // are broken by id DESC, but ids here sort the same way).
            if i < 4 {
                sqlx::query("UPDATE provider_jobs SET created_at = datetime('now', '-' || ? || ' seconds') WHERE id = ?")
                    .bind(5 - i)
                    .bind(format!("job-{i}"))
                    .execute(db.pool())
                    .await
                    .unwrap();
            }
        }
        let all = db.list_jobs(100).await.unwrap();
        assert_eq!(all.len(), 5);
        assert_eq!(all[0].id, "job-4", "newest first");
        assert_eq!(all[4].id, "job-0");
        let two = db.list_jobs(2).await.unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].id, "job-4");
        assert_eq!(two[1].id, "job-3");
    }

    #[tokio::test]
    async fn purge_expired_jobs_removes_only_expired() {
        let db = db().await;
        db.create_job("fresh", "kind", "tavily", "{}", 3600)
            .await
            .unwrap();
        db.create_job("stale", "kind", "tavily", "{}", 3600)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE provider_jobs SET expires_at = datetime('now', '-1 second') WHERE id = 'stale'",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let purged = db.purge_expired_jobs().await.unwrap();
        assert_eq!(purged, 1);
        assert!(db.get_job("stale").await.unwrap().is_none());
        assert!(db.get_job("fresh").await.unwrap().is_some());
    }
}
