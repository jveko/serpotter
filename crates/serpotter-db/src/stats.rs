use crate::{Db, DbError};
use sqlx::Row;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceStats {
    pub service: String,
    pub keys: i64,
    pub active: i64,
    pub credits_remaining_sum: Option<i64>,
    pub credits_limit_sum: Option<i64>,
}

impl Db {
    pub async fn stats_by_service(&self) -> Result<Vec<ServiceStats>, DbError> {
        let rows = sqlx::query(
            "SELECT service, \
                    COUNT(*) AS keys, \
                    COALESCE(SUM(CASE WHEN active = 1 THEN 1 ELSE 0 END), 0) AS active, \
                    SUM(credits_remaining) AS credits_remaining_sum, \
                    SUM(credits_limit) AS credits_limit_sum \
             FROM api_keys \
             GROUP BY service \
             ORDER BY service ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(ServiceStats {
                service: r.try_get("service")?,
                keys: r.try_get("keys")?,
                active: r.try_get("active")?,
                credits_remaining_sum: r.try_get("credits_remaining_sum")?,
                credits_limit_sum: r.try_get("credits_limit_sum")?,
            });
        }
        Ok(out)
    }
}
