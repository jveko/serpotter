# Multi-Account Key Load-Balance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace binary credit-tier + least-inflight key pick with Envoy-style credit-proportional score `effective_C / (inflight+1)`, soft-burn `credits_remaining` by 1 on success, and NULL mid-weight via `KEY_UNKNOWN_CREDIT_WEIGHT`.

**Architecture:** Extend `Db::acquire_api_key_shared` ORDER BY + fourth arg `unknown_credit_weight`; soft-burn in `report_api_key_success` only; `KeyPool` stores/passes unknown weight from env/`with_config`. No schema migration, no ProxyPool/wire changes. Stride/pass is out of scope (spec phase-2).

**Tech Stack:** Rust crates `serpotter-db`, `serpotter-keypool`; sqlx SQLite; existing KeyPool wait/notify; `cargo test` / `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-07-30-multi-account-key-load-balance-design.md`

## Global Constraints

- Scope: **API keys only** (per `service`); ProxyPool unchanged
- Success metric: **credit-proportional drain-even** with concurrency damping
- Pick: deterministic SQL only (no RNG / P2C / lottery)
- Soft burn: **−1 on `report_api_key_success` only** when `credits_remaining IS NOT NULL`; floor 0; never invent for NULL
- NULL mid-weight: `KEY_UNKNOWN_CREDIT_WEIGHT` default **100**, clamp ≥ 1
- Exhausted (`credits_remaining = 0`): last tier, still eligible
- Integer **SCALE = 1000** code const (not env)
- No migration / `EXPECTED_SCHEMA_VERSION` bump
- No REST/MCP/product wire change
- No Firecrawl team-dedup, no Exa/xAI residual invention, no stride columns
- Never `git commit --no-verify`
- Prefer `rtk cargo test` / `rtk cargo clippy` when available
- Conventional commits, one concern per commit
- Clean cutover: update every `acquire_api_key_shared` / `KeyPool::with_config` callsite (no dual-path shims)

## File map

| File | Responsibility |
| --- | --- |
| `crates/serpotter-db/src/lib.rs` | Export `KEY_CREDIT_SCORE_SCALE`, `DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT` |
| `crates/serpotter-db/src/keys/acquire_report.rs` | New ORDER BY + `unknown_credit_weight` arg; success soft burn |
| `crates/serpotter-db/tests/migrate.rs` | SQL-level preference, damping, NULL mid, burn, sync overwrite tests; update existing 4-arg acquires |
| `crates/serpotter-keypool/src/lib.rs` | `unknown_credit_weight` field; env; `with_config` 5th arg; pass into acquire |
| `crates/serpotter-keypool/src/tests.rs` | `pool_with` + multi-key / burn tests; update `with_config` calls |
| `crates/serpotter-api/tests/common/mod.rs` | `KeyPool::with_config` 5th arg |
| `crates/serpotter-api/tests/search_auth.rs` | `acquire_api_key_shared` 4th arg if present |
| `crates/serpotter-product/src/report.rs` | test `with_config` 5th arg |
| `docs/ops/env.md` | `KEY_UNKNOWN_CREDIT_WEIGHT` + honesty notes |
| `.env.example` | commented `KEY_UNKNOWN_CREDIT_WEIGHT` |
| `crates/serpotter-db/AGENTS.md` | acquire signature + pick policy one-liner |
| `crates/serpotter-keypool/AGENTS.md` | env + pick policy one-liner |

**Public API after cutover:**

```rust
// serpotter-db
pub const KEY_CREDIT_SCORE_SCALE: i64 = 1000;
pub const DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT: i64 = 100;

pub async fn acquire_api_key_shared(
    &self,
    service: &str,
    max_inflight: i64,
    hold_ttl_secs: i64,
    unknown_credit_weight: i64,
) -> Result<Option<ApiKeyRow>, DbError>;

// serpotter-keypool
pub fn with_config(
    db: Db,
    max_inflight: i64,
    acquire_timeout: Duration,
    hold_ttl_secs: i64,
    unknown_credit_weight: i64,
) -> Self;
```

---

### Task 1: Db constants + scored acquire + soft burn (TDD)

**Files:**
- Modify: `crates/serpotter-db/src/lib.rs`
- Modify: `crates/serpotter-db/src/keys/acquire_report.rs`
- Modify: `crates/serpotter-db/tests/migrate.rs`
- Modify (callsite compile fix only, minimal): every `acquire_api_key_shared(` in workspace to pass `DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT` so the package/workspace still compiles while implementing

**Interfaces:**
- Consumes: existing `api_keys` columns (`credits_remaining`, `inflight`, `last_used_at`, …)
- Produces: 4-arg `acquire_api_key_shared`; soft-burn success SQL; exported scale/default weight consts

- [ ] **Step 1: Add failing db tests for new pick + burn**

Append to `crates/serpotter-db/tests/migrate.rs` (keep existing tests; they will need the 4th acquire arg in Step 3):

```rust
#[tokio::test]
async fn shared_acquire_prefers_higher_credits_when_idle() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let low = db.insert_api_key("tavily", "tvly-low").await.unwrap();
    db.set_api_key_credits(low.id, Some(10)).await.unwrap();
    let high = db.insert_api_key("tavily", "tvly-high").await.unwrap();
    db.set_api_key_credits(high.id, Some(100)).await.unwrap();

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(
        acquired.id, high.id,
        "idle keys: higher credits_remaining must win"
    );
}

#[tokio::test]
async fn shared_acquire_load_damping_can_prefer_lower_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    // max_inflight=3: rich at inflight=2 → score (100*1000)/3 = 33333
    // poor at inflight=0 → score (50*1000)/1 = 50000 → poor wins
    let rich = db.insert_api_key("tavily", "tvly-rich").await.unwrap();
    db.set_api_key_credits(rich.id, Some(100)).await.unwrap();
    let poor = db.insert_api_key("tavily", "tvly-poor").await.unwrap();
    db.set_api_key_credits(poor.id, Some(50)).await.unwrap();

    sqlx::query("UPDATE api_keys SET inflight = 2 WHERE id = ?")
        .bind(rich.id)
        .execute(db.pool())
        .await
        .unwrap();

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(
        acquired.id, poor.id,
        "C/(inflight+1) must allow freer lower-credit key to beat loaded richer key"
    );
}

#[tokio::test]
async fn shared_acquire_null_before_exhausted_uses_mid_weight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let zero = db.insert_api_key("tavily", "tvly-zero").await.unwrap();
    db.set_api_key_credits(zero.id, Some(0)).await.unwrap();
    let unknown = db.insert_api_key("tavily", "tvly-null").await.unwrap();
    // credits_remaining stays NULL

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            /* unknown_weight */ 100,
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(
        acquired.id, unknown.id,
        "NULL must beat exhausted tier even when inserted later"
    );
}

#[tokio::test]
async fn shared_acquire_high_known_beats_null_mid_weight() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let unknown = db.insert_api_key("tavily", "tvly-null").await.unwrap();
    let _ = unknown;
    let high = db.insert_api_key("tavily", "tvly-high").await.unwrap();
    db.set_api_key_credits(high.id, Some(500)).await.unwrap();

    let acquired = db
        .acquire_api_key_shared(
            "tavily",
            3,
            serpotter_db::KEY_HOLD_TTL_SECS,
            100, // mid sentinel << 500
        )
        .await
        .unwrap()
        .expect("some");
    assert_eq!(acquired.id, high.id);
}

#[tokio::test]
async fn report_success_soft_burns_non_null_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(5)).await.unwrap();
    // simulate one hold so success path is realistic
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();

    db.report_api_key_success(k.id).await.unwrap();

    let rem: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rem, Some(4));
}

#[tokio::test]
async fn report_success_leaves_null_credits_null() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("exa", "exa-null").await.unwrap();
    db.acquire_api_key_shared(
        "exa",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();

    db.report_api_key_success(k.id).await.unwrap();

    let rem: Option<i64> =
        sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
            .bind(k.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rem, None);
}

#[tokio::test]
async fn report_success_never_negative_credits() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-one").await.unwrap();
    db.set_api_key_credits(k.id, Some(1)).await.unwrap();
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();
    db.report_api_key_success(k.id).await.unwrap();
    // second success without re-acquire still floors at 0 (idempotent safety)
    db.report_api_key_success(k.id).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 0);
}

#[tokio::test]
async fn update_api_key_usage_overwrites_after_soft_burn() {
    let db = serpotter_db::connect_and_migrate("sqlite::memory:")
        .await
        .expect("migrate");
    let k = db.insert_api_key("tavily", "tvly-sync").await.unwrap();
    db.set_api_key_credits(k.id, Some(10)).await.unwrap();
    db.acquire_api_key_shared(
        "tavily",
        3,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
    .await
    .unwrap()
    .unwrap();
    db.report_api_key_success(k.id).await.unwrap(); // → 9
    db.update_api_key_usage(k.id, 42, 100).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 42, "sync must overwrite soft burn");
}
```

- [ ] **Step 2: Run new tests — expect fail/compile error**

```bash
rtk cargo test -p serpotter-db shared_acquire_prefers_higher_credits -- --nocapture
```

Expected: compile error on missing `DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT` and/or wrong acquire arity, or assertion fail if only tests added before signature change.

- [ ] **Step 3: Implement constants + acquire signature + ORDER BY + soft burn; fix all workspace callsites**

In `crates/serpotter-db/src/lib.rs` after `KEY_HOLD_TTL_SECS`:

```rust
/// Integer scale for credit×load score: `(effective_C * SCALE) / (inflight + 1)`.
pub const KEY_CREDIT_SCORE_SCALE: i64 = 1000;
/// Default effective_C when `credits_remaining IS NULL` (Exa/xAI/unsynced).
pub const DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT: i64 = 100;
```

Replace `acquire_api_key_shared` in `acquire_report.rs` with:

```rust
    /// Shared-cap acquire: reclaim expired holds, pick Envoy-damped credit score under max, optimistic bump.
    ///
    /// Score (non-exhausted): `(effective_C * KEY_CREDIT_SCORE_SCALE) / (inflight + 1)` DESC.
    /// `effective_C` = `credits_remaining` if non-NULL, else `unknown_credit_weight` (clamped ≥ 1).
    /// Exhausted (`credits_remaining = 0`) is last tier but still eligible.
    pub async fn acquire_api_key_shared(
        &self,
        service: &str,
        max_inflight: i64,
        hold_ttl_secs: i64,
        unknown_credit_weight: i64,
    ) -> Result<Option<ApiKeyRow>, DbError> {
        let unknown_credit_weight = unknown_credit_weight.max(1);
        let hold_ttl_secs = hold_ttl_secs.max(1);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE api_keys SET inflight = 0, lease_until = NULL \
             WHERE lease_until IS NOT NULL AND lease_until <= datetime('now')",
        )
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(
            "SELECT id, service, key, active, consecutive_fails FROM api_keys \
             WHERE service = ? AND active = 1 AND inflight < ? \
             ORDER BY \
               CASE WHEN credits_remaining = 0 THEN 1 ELSE 0 END, \
               (CASE \
                  WHEN credits_remaining IS NULL THEN ? \
                  ELSE credits_remaining \
                END * ?) / (inflight + 1) DESC, \
               last_used_at IS NOT NULL, last_used_at ASC, id ASC \
             LIMIT 1",
        )
        .bind(service)
        .bind(max_inflight)
        .bind(unknown_credit_weight)
        .bind(crate::KEY_CREDIT_SCORE_SCALE)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(r) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: i64 = r.try_get("id")?;
        let updated = sqlx::query(
            "UPDATE api_keys SET \
                inflight = inflight + 1, \
                last_used_at = datetime('now'), \
                lease_until = datetime('now', '+' || ? || ' seconds') \
             WHERE id = ? AND active = 1 AND inflight < ?",
        )
        .bind(hold_ttl_secs)
        .bind(id)
        .bind(max_inflight)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            tx.commit().await?;
            return Ok(None);
        }
        tx.commit().await?;
        Ok(Some(ApiKeyRow {
            id,
            service: r.try_get("service")?,
            key: r.try_get("key")?,
            active: r.try_get("active")?,
            consecutive_fails: r.try_get("consecutive_fails")?,
        }))
    }
```

Replace `report_api_key_success` body SQL with:

```rust
    pub async fn report_api_key_success(&self, id: i64) -> Result<(), DbError> {
        sqlx::query(
            "UPDATE api_keys SET \
                consecutive_fails = 0, \
                last_used_at = datetime('now'), \
                credits_remaining = CASE \
                  WHEN credits_remaining IS NULL THEN NULL \
                  WHEN credits_remaining <= 0 THEN 0 \
                  ELSE credits_remaining - 1 \
                END, \
                inflight = CASE WHEN inflight > 0 THEN inflight - 1 ELSE 0 END, \
                lease_until = CASE WHEN inflight <= 1 THEN NULL ELSE lease_until END \
             WHERE id = ?",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
```

**Callsite sweep** (pass `serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT` as 4th arg):

- All `acquire_api_key_shared(` in `crates/serpotter-db/tests/migrate.rs`
- `crates/serpotter-keypool/src/lib.rs` (two call sites) — temporarily hardcode default until Task 2 wires field
- `crates/serpotter-api/tests/search_auth.rs` if it calls db acquire directly

Do **not** change `report_failure` / `release` / `report_exhausted` credit logic.

- [ ] **Step 4: Run db tests — expect pass**

```bash
rtk cargo test -p serpotter-db
rtk cargo clippy -p serpotter-db -- -D warnings
```

Expected: all migrate tests green including new ones; clippy clean.

- [ ] **Step 5: Commit**

```bash
rtk git add crates/serpotter-db/src/lib.rs \
  crates/serpotter-db/src/keys/acquire_report.rs \
  crates/serpotter-db/tests/migrate.rs \
  crates/serpotter-keypool/src/lib.rs \
  crates/serpotter-api/tests/search_auth.rs
rtk git commit -m "$(cat <<'EOF'
feat(db): credit-damped key pick and soft burn on success

ORDER BY C/(inflight+1) with NULL mid-weight; decrement
credits_remaining on report_success; no schema change.
EOF
)"
```

(If keypool still fails compile because of arity, include the minimal 4th-arg default in the same commit so the workspace builds — Task 2 then wires config properly.)

---

### Task 2: KeyPool unknown-weight config + pool tests

**Files:**
- Modify: `crates/serpotter-keypool/src/lib.rs`
- Modify: `crates/serpotter-keypool/src/tests.rs`
- Modify: `crates/serpotter-api/tests/common/mod.rs`
- Modify: `crates/serpotter-product/src/report.rs` (test-only `with_config` calls)

**Interfaces:**
- Consumes: `Db::acquire_api_key_shared(..., unknown_credit_weight)`
- Produces: `KeyPool::with_config(..., unknown_credit_weight: i64)`; `new()` reads `KEY_UNKNOWN_CREDIT_WEIGHT`; `unknown_credit_weight()` getter optional

- [ ] **Step 1: Write failing/updated pool tests**

Update helper:

```rust
fn pool_with(db: Db, max_inflight: i64, timeout: Duration) -> KeyPool {
    KeyPool::with_config(
        db,
        max_inflight,
        timeout,
        serpotter_db::KEY_HOLD_TTL_SECS,
        serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
    )
}

fn pool_with_unknown(
    db: Db,
    max_inflight: i64,
    timeout: Duration,
    unknown: i64,
) -> KeyPool {
    KeyPool::with_config(
        db,
        max_inflight,
        timeout,
        serpotter_db::KEY_HOLD_TTL_SECS,
        unknown,
    )
}
```

Append:

```rust
#[tokio::test]
async fn acquire_prefers_higher_credits_when_idle() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let low = db.insert_api_key("tavily", "tvly-low").await.unwrap();
    db.set_api_key_credits(low.id, Some(10)).await.unwrap();
    let high = db.insert_api_key("tavily", "tvly-high").await.unwrap();
    db.set_api_key_credits(high.id, Some(100)).await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));

    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, high.id);
    pool.report_success(lease.id).await.unwrap();
}

#[tokio::test]
async fn report_success_soft_burns_via_pool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(3)).await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    pool.report_success(lease.id).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 2);
}

#[tokio::test]
async fn release_does_not_soft_burn() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("tavily", "tvly-rel-burn").await.unwrap();
    db.set_api_key_credits(k.id, Some(7)).await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));
    let lease = pool.acquire("tavily").await.unwrap();
    pool.release(lease.id).await.unwrap();
    let rem: i64 = sqlx::query_scalar("SELECT credits_remaining FROM api_keys WHERE id = ?")
        .bind(k.id)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(rem, 7);
}

#[tokio::test]
async fn custom_unknown_weight_affects_null_vs_low_known() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let known = db.insert_api_key("tavily", "tvly-known").await.unwrap();
    db.set_api_key_credits(known.id, Some(5)).await.unwrap();
    let unknown = db.insert_api_key("tavily", "tvly-unk").await.unwrap();
    let _ = unknown;
    // unknown_weight=1 → known (5) wins; if weight were 1000, unknown would win
    let pool = pool_with_unknown(db, 3, Duration::from_secs(5), 1);
    let lease = pool.acquire("tavily").await.unwrap();
    assert_eq!(lease.id, known.id);
    pool.report_success(lease.id).await.unwrap();
}
```

Existing `report_exhausted_prefers_other_key` must still pass under continuous score (C=0 last tier).

- [ ] **Step 2: Run keypool tests — expect compile fail on `with_config` arity**

```bash
rtk cargo test -p serpotter-keypool acquire_prefers_higher_credits
```

Expected: `with_config` takes 5 args / missing field.

- [ ] **Step 3: Implement KeyPool field + with_config + from_env**

```rust
const DEFAULT_UNKNOWN_CREDIT_WEIGHT: i64 = serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT;

pub struct KeyPool {
    db: Db,
    lock: Mutex<()>,
    notify: Notify,
    max_inflight: i64,
    acquire_timeout: Duration,
    hold_ttl_secs: i64,
    unknown_credit_weight: i64,
}

impl KeyPool {
    pub fn new(db: Db) -> Self {
        Self::with_config(
            db,
            env_i64("KEY_MAX_INFLIGHT", DEFAULT_MAX_INFLIGHT),
            Duration::from_secs(env_u64(
                "KEY_ACQUIRE_TIMEOUT_SECS",
                DEFAULT_ACQUIRE_TIMEOUT_SECS,
            )),
            env_i64("KEY_HOLD_TTL_SECS", serpotter_db::KEY_HOLD_TTL_SECS),
            env_i64("KEY_UNKNOWN_CREDIT_WEIGHT", DEFAULT_UNKNOWN_CREDIT_WEIGHT),
        )
    }

    pub fn with_config(
        db: Db,
        max_inflight: i64,
        acquire_timeout: Duration,
        hold_ttl_secs: i64,
        unknown_credit_weight: i64,
    ) -> Self {
        Self {
            db,
            lock: Mutex::new(()),
            notify: Notify::new(),
            max_inflight: max_inflight.max(1),
            acquire_timeout,
            hold_ttl_secs: hold_ttl_secs.max(1),
            unknown_credit_weight: unknown_credit_weight.max(1),
        }
    }

    pub fn unknown_credit_weight(&self) -> i64 {
        self.unknown_credit_weight
    }
```

Both acquire call sites:

```rust
.db.acquire_api_key_shared(
    service,
    self.max_inflight,
    self.hold_ttl_secs,
    self.unknown_credit_weight,
)
```

Update doc comment on `new` to list `KEY_UNKNOWN_CREDIT_WEIGHT`.

**Callsites for 5-arg `with_config`:**

```rust
// crates/serpotter-api/tests/common/mod.rs
KeyPool::with_config(
    db.clone(),
    max_inflight,
    acquire_timeout,
    hold_ttl_secs,
    serpotter_db::DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT,
)

// crates/serpotter-product/src/report.rs tests — same 5th arg default
```

Grep `KeyPool::with_config` workspace-wide and fix every hit.

- [ ] **Step 4: Run tests — expect pass**

```bash
rtk cargo test -p serpotter-keypool
rtk cargo test -p serpotter-db
rtk cargo test -p serpotter-product report
rtk cargo test -p serpotter-api
rtk cargo clippy -p serpotter-keypool -p serpotter-db -p serpotter-product -p serpotter-api -- -D warnings
```

Expected: all green.

- [ ] **Step 5: Commit**

```bash
rtk git add crates/serpotter-keypool/src/lib.rs \
  crates/serpotter-keypool/src/tests.rs \
  crates/serpotter-api/tests/common/mod.rs \
  crates/serpotter-product/src/report.rs
rtk git commit -m "$(cat <<'EOF'
feat(keypool): wire KEY_UNKNOWN_CREDIT_WEIGHT into acquire

Pass mid-weight sentinel into scored shared acquire; soft burn
covered by pool success path tests.
EOF
)"
```

---

### Task 3: Ops docs + crate AGENTS

**Files:**
- Modify: `docs/ops/env.md`
- Modify: `.env.example`
- Modify: `crates/serpotter-db/AGENTS.md`
- Modify: `crates/serpotter-keypool/AGENTS.md`

**Interfaces:** none (docs only)

- [ ] **Step 1: Update env docs**

In `docs/ops/env.md` key-pool table, add row after `KEY_HOLD_TTL_SECS`:

```markdown
| `KEY_UNKNOWN_CREDIT_WEIGHT` | `100` | effective credit weight when `credits_remaining IS NULL` (Exa/xAI/unsynced). Used in pick score `(C * 1000) / (inflight + 1)`. Clamp ≥ 1. |
```

After the key-pool table (or in the credits paragraph), add honesty notes:

```markdown
**Multi-key pick:** active keys under cap are ordered exhausted-last, then `(effective_credits * 1000) / (inflight + 1)` DESC, then LRU. Successful holds soft-decrement non-NULL `credits_remaining` by 1 (rank heuristic; Tavily/Firecrawl sync overwrites). Soft −1 is not billing truth (Tavily advanced/research and Firecrawl multi-credit ops differ). Firecrawl usage residual is **team-wide** — multiple keys on one team each storing full remaining can overstate capacity. Tavily `GET /usage` is limited to **10 calls / 10 minutes** — avoid thrashing multi-key credit sync.
```

In `.env.example` key pool block:

```bash
# KEY_UNKNOWN_CREDIT_WEIGHT=100
```

- [ ] **Step 2: Update AGENTS one-liners**

`serpotter-db/AGENTS.md` key acquire row:

```markdown
| Key acquire (shared) | `acquire_api_key_shared(service, max_inflight, hold_ttl_secs, unknown_credit_weight)` — exhausted last, score `(C*1000)/(inflight+1)`; success soft-burns non-NULL credits −1 |
```

`serpotter-keypool/AGENTS.md`:

- Env limits row: add `KEY_UNKNOWN_CREDIT_WEIGHT=100`
- Report outcome: note success soft-burns credits via db
- Updated date line

- [ ] **Step 3: No code test required — skim for accuracy**

Confirm docs match shipped SQL (exhausted `= 0` only; NULL mid-weight; scale 1000).

- [ ] **Step 4: Commit**

```bash
rtk git add docs/ops/env.md .env.example \
  crates/serpotter-db/AGENTS.md \
  crates/serpotter-keypool/AGENTS.md
rtk git commit -m "$(cat <<'EOF'
docs(ops): document credit-damped key pick and unknown weight

KEY_UNKNOWN_CREDIT_WEIGHT, soft-burn honesty, Tavily/Firecrawl caveats.
EOF
)"
```

---

### Task 4: Workspace verification gate

**Files:** none (verify only)

- [ ] **Step 1: Full quality gate for touched surface**

```bash
rtk cargo test --workspace
rtk cargo clippy --workspace -- -D warnings
```

Expected: all pass. If any old test assumed binary credit bucket + pure least-inflight order among equal healthy keys, update assertion to continuous score semantics (do not weaken contracts).

- [ ] **Step 2: Grep for stale 3-arg acquire / 4-arg with_config**

```bash
# should find only the new 4-arg form
rtk grep -n "acquire_api_key_shared" crates
rtk grep -n "KeyPool::with_config" crates
```

Expected: every call has unknown weight / 5th config arg.

- [ ] **Step 3: Final commit only if Step 1 forced fixes**

```bash
rtk git add -u
rtk git commit -m "fix(keypool): align residual tests with credit-damped pick"
```

(Skip commit if already green with no extra diffs.)

---

## Spec coverage checklist

| Spec requirement | Task |
| --- | --- |
| Envoy `C/(inflight+1)` ORDER BY | T1 |
| Exhausted last, still eligible | T1 (SQL + existing tests) |
| NULL mid-weight env default 100 | T1 + T2 |
| SCALE 1000 const | T1 |
| Soft −1 on success only | T1 + T2 |
| No burn on release/failure | T2 release test; failure SQL untouched |
| Sync overwrites burn | T1 |
| KeyPool wait/notify unchanged | T2 (no loop change) |
| No schema/wire/ProxyPool | all tasks |
| Ops env + honesty notes | T3 |
| Stride phase-2 not implemented | explicit omission |
| Tests: rich idle, damping, NULL, burn, sync | T1–T2 |
| Callsite clean cutover | T1–T2 + T4 grep |

## Placeholder / consistency self-check

- No TBD/TODO in steps
- `acquire_api_key_shared` always 4 service-facing args after T1
- `with_config` always 5 args after T2
- Const names: `KEY_CREDIT_SCORE_SCALE`, `DEFAULT_KEY_UNKNOWN_CREDIT_WEIGHT`, env `KEY_UNKNOWN_CREDIT_WEIGHT`

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-30-multi-account-key-load-balance.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)

**2. Parallel Independent Domains** — only if you split docs (T3) from code; T1→T2 are strictly sequential (API arity)

**Which approach?**
