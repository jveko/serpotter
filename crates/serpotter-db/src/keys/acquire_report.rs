use super::rows::ApiKeyRow;
use crate::{Db, DbError, MAX_CONSECUTIVE_FAILURES};
use sqlx::Row;

#[derive(Clone, Debug)]
pub struct KeyLease {
    pub key: ApiKeyRow,
    pub token: i64,
}

impl std::ops::Deref for KeyLease {
    type Target = ApiKeyRow;
    fn deref(&self) -> &Self::Target {
        &self.key
    }
}

/// Post-report state of the key row a health report just touched.
///
/// The report fns used to answer only "did the lease exist" (`bool`), which
/// says nothing about the row afterwards. Every derivable fact needs a PRE
/// value, because a health report is IDEMPOTENT and leases OVERLAP: several
/// legs can hold the same key concurrently (`max_inflight` defaults to 3), so
/// legs B and C can finish AFTER leg A already flipped the row — a post-state
/// check alone would report that one flip two or three times. Hence the PRE
/// values read in the SAME transaction and the POST values returned by
/// `RETURNING`; a caller derives a transition from the pair, never from the
/// post value alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPostState {
    /// The lease token resolved to a live key row. `false` = the lease was
    /// already reclaimed (TTL expiry / double finish) and NOTHING was written.
    pub existed: bool,
    /// `active` AFTER the report.
    pub active: bool,
    /// `active` BEFORE the report. `true` plus a `false` post value is the
    /// only proof that THIS report is what took the row out of rotation.
    pub active_before: bool,
    pub consecutive_fails: i64,
    /// `credits_remaining` AFTER the report; `None` = untracked credits
    /// (never conflated with 0).
    pub credits_remaining: Option<i64>,
    /// `credits_remaining` read in the SAME transaction immediately BEFORE
    /// the UPDATE. Comparing it against the POST value is the only way to
    /// spot a real zeroing rather than a no-op rewrite of an existing 0.
    pub credits_before: Option<i64>,
}

impl KeyPostState {
    /// The lease (or its row) was gone: no write happened, every field is a
    /// placeholder and `existed` is the only meaningful one.
    pub const MISSING: Self = Self {
        existed: false,
        active: false,
        active_before: false,
        consecutive_fails: 0,
        credits_remaining: None,
        credits_before: None,
    };
}

/// The health verdicts a lease release can apply. A private enum (not a `&str`)
/// so the verdict is a closed set at the type level: the SQL match is then
/// exhaustive instead of needing a runtime "unknown verdict" error path that
/// no caller can ever reach.
///
/// [`KeyHealth::sql`] is the SINGLE definition of each verdict's write, and it
/// is the only place that text appears. The lease-report path reads the
/// `RETURNING` columns out of it to build a [`KeyPostState`]; the health-note
/// path (`note_key_health_failure`) executes the very same statement and
/// discards the returned row. That is what keeps the two paths from drifting:
/// the fail@3 flip predicate and the `auth_fail` stamp invariant (db
/// AGENTS.md) is enforced by construction, not by review.
#[derive(Clone, Copy)]
enum KeyHealth {
    Success,
    Failure,
    /// A vendor 429. The cooldown length travels IN the verdict, not as a
    /// separate argument: `sql()` owns the placeholder and `bind_args()`
    /// owns the value, so the two cannot drift apart. A mismatch would not
    /// be a compile error — it would surface at runtime as a "wrong number
    /// of parameters" from the driver, long after the write site.
    Exhausted {
        cooldown_secs: i64,
    },
    PaymentRequired,
    Suspended,
}

impl KeyHealth {
    fn sql(self) -> &'static str {
        match self {
            Self::Success => "UPDATE api_keys SET consecutive_fails = 0, last_used_at = datetime('now'), credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL WHEN credits_remaining <= 0 THEN 0 ELSE credits_remaining - 1 END WHERE id = ? RETURNING active, consecutive_fails, credits_remaining",
            Self::Failure => "UPDATE api_keys SET consecutive_fails = consecutive_fails + 1, last_used_at = datetime('now'), active = CASE WHEN consecutive_fails + 1 >= ? THEN 0 ELSE active END, disabled_reason = CASE WHEN disabled_reason IS NULL AND consecutive_fails + 1 >= ? THEN 'auth_fail' ELSE disabled_reason END WHERE id = ? RETURNING active, consecutive_fails, credits_remaining",
            Self::Exhausted { .. } => "UPDATE api_keys SET credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL ELSE 0 END, last_used_at = datetime('now'), cooldown_until = datetime('now', '+' || ? || ' seconds') WHERE id = ? RETURNING active, consecutive_fails, credits_remaining",
            Self::PaymentRequired => "UPDATE api_keys SET credits_remaining = 0, last_used_at = datetime('now') WHERE id = ? RETURNING active, consecutive_fails, credits_remaining",
            Self::Suspended => "UPDATE api_keys SET active = 0, disabled_reason = 'vendor_suspended', last_used_at = datetime('now') WHERE id = ? RETURNING active, consecutive_fails, credits_remaining",
        }
    }

    /// Bind every placeholder [`Self::sql`] declares, in order, except the
    /// trailing key id (the caller binds that). One definition, so a verdict
    /// can never hand `sql()` a statement whose `?` count it does not match.
    fn bind_args<'q>(
        self,
        q: sqlx::query::Query<'q, sqlx::Sqlite, <sqlx::Sqlite as sqlx::Database>::Arguments>,
        max_fails: i64,
    ) -> sqlx::query::Query<'q, sqlx::Sqlite, <sqlx::Sqlite as sqlx::Database>::Arguments> {
        match self {
            // The fail@3 flip and the `auth_fail` stamp share one statement
            // and one `max_fails` bound, so the two predicates can never
            // drift apart (see the db AGENTS.md invariants).
            Self::Failure => q.bind(max_fails).bind(max_fails),
            // Bound in the SAME statement as the credits zeroing: the two
            // writes a 429 causes are atomic, so a leg can never leave
            // credits zeroed with no cooldown stamp (or the reverse).
            // `cooldown_until` is write-once-per-report and NEVER cleared
            // elsewhere: the acquire path reads it as a demotion tier.
            // Deliberate SECOND clamp (report_key_token pre-clamps too): the
            // bind sits where it knows the placeholder's sign requirement, so
            // a negative can never reach the SQL even via a future caller.
            Self::Exhausted { cooldown_secs } => q.bind(cooldown_secs.max(0)),
            Self::Success | Self::PaymentRequired | Self::Suspended => q,
        }
    }
}

const SQL_RECLAIM_KEY_LEASES: &str =
    "DELETE FROM api_key_leases WHERE lease_until <= datetime('now')";
const SQL_RECONCILE_KEYS: &str = "UPDATE api_keys SET \
    inflight = (SELECT COUNT(*) FROM api_key_leases WHERE api_key_id = api_keys.id), \
    lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = api_keys.id) \
    WHERE inflight != (SELECT COUNT(*) FROM api_key_leases WHERE api_key_id = api_keys.id) \
       OR lease_until IS NOT (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = api_keys.id)";

impl Db {
    async fn reclaim_key_leases<'e, E>(executor: E) -> Result<u64, DbError>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        Ok(sqlx::query(SQL_RECLAIM_KEY_LEASES)
            .execute(executor)
            .await?
            .rows_affected())
    }

    pub async fn reclaim_expired_key_holds(&self) -> Result<u64, DbError> {
        let mut tx = self.pool.begin().await?;
        let removed = Self::reclaim_key_leases(&mut *tx).await?;
        sqlx::query(SQL_RECONCILE_KEYS).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(removed)
    }

    pub async fn zero_all_key_inflight(&self) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM api_key_leases")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE api_keys SET inflight = 0, lease_until = NULL")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn acquire_api_key_shared(
        &self,
        service: &str,
        max_inflight: i64,
        hold_ttl_secs: i64,
        unknown_credit_weight: i64,
    ) -> Result<Option<KeyLease>, DbError> {
        let unknown_credit_weight = unknown_credit_weight.max(1);
        let hold_ttl_secs = hold_ttl_secs.max(1);
        let mut tx = self.pool.begin().await?;
        Self::reclaim_key_leases(&mut *tx).await?;
        sqlx::query(SQL_RECONCILE_KEYS).execute(&mut *tx).await?;
        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint FROM api_keys \
             WHERE service = ? AND active = 1 AND inflight < ? \
             ORDER BY CASE WHEN cooldown_until IS NOT NULL AND cooldown_until > datetime('now') THEN 1 ELSE 0 END, \
               CASE WHEN credits_remaining = 0 THEN 1 ELSE 0 END, \
               (CASE WHEN credits_remaining IS NULL THEN ? ELSE credits_remaining END * ?) / (inflight + 1) DESC, \
               last_used_at IS NOT NULL, last_used_at ASC, id ASC LIMIT 1",
        ).bind(service).bind(max_inflight).bind(unknown_credit_weight)
         .bind(crate::KEY_CREDIT_SCORE_SCALE).fetch_optional(&mut *tx).await?;
        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: i64 = r.try_get("id")?;
        let updated = sqlx::query(
            "UPDATE api_keys SET inflight = inflight + 1, last_used_at = datetime('now') \
             WHERE id = ? AND active = 1 AND inflight < ?",
        )
        .bind(id)
        .bind(max_inflight)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }
        let token: i64 = sqlx::query_scalar(
            "INSERT INTO api_key_leases(api_key_id, lease_until) VALUES (?, datetime('now', '+' || ? || ' seconds')) RETURNING token",
        ).bind(id).bind(hold_ttl_secs).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE api_keys SET lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = ?) WHERE id = ?")
            .bind(id).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(KeyLease {
            token,
            key: ApiKeyRow {
                id,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
                key_fingerprint: r.try_get("key_fingerprint")?,
            },
        }))
    }

    /// Delete the lease row named by `token`, returning its parent key id.
    /// `None` = the lease was already reclaimed (never an error: a holder may
    /// simply be late).
    async fn take_lease_token(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        token: i64,
    ) -> Result<Option<i64>, DbError> {
        Ok(
            sqlx::query_scalar("DELETE FROM api_key_leases WHERE token = ? RETURNING api_key_id")
                .bind(token)
                .fetch_optional(&mut **tx)
                .await?,
        )
    }

    /// Recompute the parent row's shared inflight / lease_until from the leases
    /// still held (multi-hold keys: only the LAST release clears the stamp).
    async fn reconcile_lease_hold(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: i64,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET inflight = (SELECT COUNT(*) FROM api_key_leases WHERE api_key_id = ?), lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = ?) WHERE id = ?")
            .bind(id).bind(id).bind(id).execute(&mut **tx).await?;
        Ok(())
    }

    /// Release the lease with no health write (tunnel / cancel paths): just
    /// free the shared slot.
    pub async fn release_api_key_lease(&self, token: i64) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let Some(id) = Self::take_lease_token(&mut tx, token).await? else {
            tx.commit().await?;
            return Ok(false);
        };
        Self::reconcile_lease_hold(&mut tx, id).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Apply a health verdict to the leased row and return its post state.
    ///
    /// ONE transaction: delete the lease row, read the PRE
    /// `credits_remaining`, run the health UPDATE with `RETURNING active,
    /// consecutive_fails, credits_remaining`, then reconcile the shared
    /// inflight. The PRE read MUST stay inside this transaction — outside it,
    /// a concurrent holder's report could be observed and the "who zeroed the
    /// credits" answer would be wrong.
    async fn report_key_token(
        &self,
        token: i64,
        health: KeyHealth,
        max_fails: i64,
    ) -> Result<KeyPostState, DbError> {
        let mut tx = self.pool.begin().await?;
        let Some(id) = Self::take_lease_token(&mut tx, token).await? else {
            tx.commit().await?;
            return Ok(KeyPostState::MISSING);
        };
        // PRE read of BOTH flip-prone columns in one statement, inside this
        // transaction: `active` decides whether the disable flipped HERE, and
        // `credits_remaining` whether the credits were zeroed HERE.
        let pre = sqlx::query("SELECT active, credits_remaining FROM api_keys WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let (active_before, credits_before) = match pre {
            Some(r) => (
                r.try_get::<bool, _>("active")?,
                r.try_get::<Option<i64>, _>("credits_remaining")?,
            ),
            None => (false, None),
        };
        let post = health
            .bind_args(sqlx::query(health.sql()), max_fails)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        let post = post
            .map(|r| {
                Ok::<(bool, i64, Option<i64>), DbError>((
                    r.try_get::<bool, _>("active")?,
                    r.try_get::<i64, _>("consecutive_fails")?,
                    r.try_get::<Option<i64>, _>("credits_remaining")?,
                ))
            })
            .transpose()?;
        Self::reconcile_lease_hold(&mut tx, id).await?;
        tx.commit().await?;
        Ok(match post {
            Some((active, consecutive_fails, credits_remaining)) => KeyPostState {
                existed: true,
                active,
                active_before,
                consecutive_fails,
                credits_remaining,
                credits_before,
            },
            // The lease pointed at a row that vanished underneath it: the
            // UPDATE matched nothing, so nothing was written.
            None => KeyPostState::MISSING,
        })
    }

    pub async fn report_api_key_success_lease(&self, token: i64) -> Result<bool, DbError> {
        Ok(self
            .report_key_token(token, KeyHealth::Success, MAX_CONSECUTIVE_FAILURES)
            .await?
            .existed)
    }
    pub async fn report_api_key_failure_lease(&self, token: i64) -> Result<KeyPostState, DbError> {
        self.report_key_token(token, KeyHealth::Failure, MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn report_api_key_exhausted_lease(
        &self,
        token: i64,
        cooldown_secs: i64,
    ) -> Result<KeyPostState, DbError> {
        self.report_key_token(
            token,
            KeyHealth::Exhausted {
                cooldown_secs: cooldown_secs.max(0),
            },
            MAX_CONSECUTIVE_FAILURES,
        )
        .await
    }

    pub async fn report_api_key_payment_required_lease(
        &self,
        token: i64,
    ) -> Result<KeyPostState, DbError> {
        self.report_key_token(token, KeyHealth::PaymentRequired, MAX_CONSECUTIVE_FAILURES)
            .await
    }
    pub async fn suspend_api_key_lease(&self, token: i64) -> Result<KeyPostState, DbError> {
        self.report_key_token(token, KeyHealth::Suspended, MAX_CONSECUTIVE_FAILURES)
            .await
    }

    /// `api_keys.cooldown_until` for `id`, or `None` when the key was never
    /// stamped (or the row is gone). The stamp is written once per exhausted
    /// report and is never cleared here; the acquire ORDER BY only compares
    /// it against `datetime('now')`.
    ///
    /// The `CASE` is load-bearing: a bare `query_scalar::<Option<String>>`
    /// on a NULL column yields `Some("")`, which would report a mark on a
    /// never-stamped key. SQL NULL and "no mark" must stay the same state.
    #[doc(hidden)] // cross-crate TEST-ONLY accessor (house convention: api's test re-exports)
    pub async fn get_api_key_cooldown(&self, id: i64) -> Result<Option<String>, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT CASE WHEN cooldown_until IS NULL THEN NULL ELSE cooldown_until END \
             FROM api_keys WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    /// Server-side `datetime('now')`, so a caller can age a `cooldown_until`
    /// without trusting its own wall clock against the db's.
    #[doc(hidden)] // cross-crate TEST-ONLY accessor (house convention: api's test re-exports)
    pub async fn now(&self) -> Result<String, DbError> {
        Ok(sqlx::query_scalar("SELECT datetime('now')")
            .fetch_one(&self.pool)
            .await?)
    }

    pub async fn count_active_keys(&self, service: &str) -> Result<i64, DbError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE service = ? AND active = 1")
                .bind(service)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    pub async fn refresh_api_key_lease(
        &self,
        token: i64,
        hold_ttl_secs: i64,
    ) -> Result<bool, DbError> {
        let mut tx = self.pool.begin().await?;
        let changed = sqlx::query("UPDATE api_key_leases SET lease_until = datetime('now', '+' || ? || ' seconds') WHERE token = ?")
            .bind(hold_ttl_secs.max(1)).bind(token).execute(&mut *tx).await?.rows_affected() > 0;
        if changed {
            sqlx::query("UPDATE api_keys SET lease_until = (SELECT MAX(lease_until) FROM api_key_leases WHERE api_key_id = api_keys.id) WHERE id = (SELECT api_key_id FROM api_key_leases WHERE token = ?)").bind(token).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(changed)
    }

    pub async fn set_api_key_lease_until(
        &self,
        id: i64,
        lease_until: Option<&str>,
    ) -> Result<(), DbError> {
        let mut tx = self.pool.begin().await?;
        match lease_until {
            Some(v) => {
                sqlx::query("UPDATE api_key_leases SET lease_until = ? WHERE api_key_id = ?")
                    .bind(v)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            None => {
                sqlx::query("DELETE FROM api_key_leases WHERE api_key_id = ?")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        sqlx::query("UPDATE api_keys SET lease_until = ? WHERE id = ?")
            .bind(lease_until)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_active_keys_for_service(
        &self,
        service: &str,
    ) -> Result<Vec<ApiKeyRow>, DbError> {
        let rows = sqlx::query("SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint FROM api_keys WHERE service = ? AND active = 1 ORDER BY usage_synced_at IS NOT NULL, usage_synced_at ASC, id ASC")
            .bind(service).fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|r| {
                Ok(ApiKeyRow {
                    id: r.try_get("id")?,
                    service: r.try_get("service")?,
                    key: r.try_get("key")?,
                    active: r.try_get("active")?,
                    consecutive_fails: r.try_get("consecutive_fails")?,
                    key_fingerprint: r.try_get("key_fingerprint")?,
                })
            })
            .collect()
    }
    pub async fn get_api_key(&self, id: i64) -> Result<Option<ApiKeyRow>, DbError> {
        let r = sqlx::query("SELECT id, service, key, active, consecutive_fails, COALESCE(key_fingerprint, '') AS key_fingerprint FROM api_keys WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        r.map(|r| {
            Ok(ApiKeyRow {
                id: r.try_get("id")?,
                service: r.try_get("service")?,
                key: r.try_get("key")?,
                active: r.try_get("active")?,
                consecutive_fails: r.try_get("consecutive_fails")?,
                key_fingerprint: r.try_get("key_fingerprint")?,
            })
        })
        .transpose()
    }
    pub async fn set_api_key_last_used_at(
        &self,
        id: i64,
        value: Option<&str>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
            .bind(value)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_success_lease`.
    pub async fn note_key_health_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET consecutive_fails = 0, last_used_at = datetime('now'), credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL WHEN credits_remaining <= 0 THEN 0 ELSE credits_remaining - 1 END WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_failure_lease`.
    pub async fn note_key_health_failure(&self, id: i64) -> Result<(), DbError> {
        // The SAME statement `report_api_key_failure_lease` runs: one
        // definition of the fail@3 flip + `auth_fail` stamp, so the two paths
        // cannot drift (the db AGENTS.md invariant, enforced by construction).
        // This is a note, not a report: the RETURNING row is discarded, since
        // the caller holds no lease and gets no KeyPostState.
        sqlx::query(KeyHealth::Failure.sql())
            .bind(MAX_CONSECUTIVE_FAILURES)
            .bind(MAX_CONSECUTIVE_FAILURES)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_exhausted_lease`.
    ///
    /// Deliberately stamps NO `cooldown_until`. This is the only exhausted
    /// write with no observed vendor 429 behind it, so there is no
    /// `Retry-After` to honour and no rate-limit window to record; a
    /// synthetic stamp here would park a key on a guess. Hence the literal
    /// below rather than `KeyHealth::Exhausted`'s SQL: the two paths are
    /// deliberately DIFFERENT statements, and `bind_args` is unreachable
    /// because this one binds only the trailing id.
    pub async fn note_key_health_exhausted(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET credits_remaining = CASE WHEN credits_remaining IS NULL THEN NULL ELSE 0 END, last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only note; never releases a lease. Prefer `report_api_key_payment_required_lease`.
    pub async fn note_key_health_payment_required(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET credits_remaining = 0, last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
    /// Health-only suspension note; never releases a lease.
    pub async fn note_key_health_suspended(&self, id: i64) -> Result<(), DbError> {
        sqlx::query("UPDATE api_keys SET active = 0, disabled_reason = 'vendor_suspended', last_used_at = datetime('now') WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }
}
