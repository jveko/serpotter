# Firecrawl Banned-Key Auto-Delete Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** On Firecrawl search/extract, detect ban-body 401/403 responses and hard-DELETE that `api_keys` row on first match so banned keys never burn fail@3 or return via re-enable cron.

**Architecture:** Pure classifier `is_firecrawl_banned(status, body)` in product; `KeyPool::report_banned` → existing `Db::delete_api_key` + notify; `KeyHold::finish_banned`; insert a ban branch in `run_provider` and `extract_url` **before** generic 401/403 → `finish_failure`. Firecrawl only; no schema migration; no wire change.

**Tech Stack:** Rust workspace crates `serpotter-product`, `serpotter-keypool`, `serpotter-db` (reuse delete); `cargo test` / `clippy -D warnings`.

**Spec:** `docs/superpowers/specs/2026-07-30-firecrawl-banned-key-delete-design.md`

## Global Constraints

- Provider scope: **Firecrawl only**
- Signal: status ∈ {401, 403} **and** case-insensitive body contains ban markers (`account has been banned` and/or `has been banned`)
- Action: **hard DELETE** on **first** match (no fail@3 wait, no fail++)
- Paths: search `run_provider` + extract `try_extract_provider` only
- Reuse `Db::delete_api_key`; no new migration/columns
- Never log full API key
- Non-ban 401/403 keep existing `finish_failure` / fail@3
- 402/429 stay on `is_exhausted_status` path (evaluate exhausted **before** ban)
- No credit-sync sweep, no other providers, no admin UI
- Never `git commit --no-verify`
- Prefer `rtk cargo test` / `rtk cargo clippy` when available
- Conventional commits, one concern per commit

## File map

| File | Responsibility |
| --- | --- |
| `crates/serpotter-product/src/search/banned.rs` | `is_firecrawl_banned` + unit tests (fixture body) |
| `crates/serpotter-product/src/search/mod.rs` | `mod banned;` + `pub use banned::is_firecrawl_banned` |
| `crates/serpotter-product/src/lib.rs` | Re-export `is_firecrawl_banned` next to `is_exhausted_status` |
| `crates/serpotter-keypool/src/lib.rs` | `report_banned(id)` |
| `crates/serpotter-keypool/src/tests.rs` | Pool delete + missing-id tests |
| `crates/serpotter-product/src/hold.rs` | `KeyHold::finish_banned` + `key_id()` |
| `crates/serpotter-product/src/search/run_provider.rs` | Ban branch before generic 401/403 |
| `crates/serpotter-product/src/extract/extract_url.rs` | Same ban branch |

**Canonical ban fixture body** (must appear in classifier tests):

```json
{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}
```

---

### Task 1: `is_firecrawl_banned` classifier

**Files:**
- Create: `crates/serpotter-product/src/search/banned.rs`
- Modify: `crates/serpotter-product/src/search/mod.rs`
- Modify: `crates/serpotter-product/src/lib.rs`

**Interfaces:**
- Consumes: nothing (pure)
- Produces: `pub fn is_firecrawl_banned(status: u16, body: &str) -> bool`

- [ ] **Step 1: Add module with failing tests first (TDD)**

Create `crates/serpotter-product/src/search/banned.rs`:

```rust
//! Firecrawl permanent-ban body detection (on-path key delete).

/// Live Firecrawl ban body (credit-usage / search / extract), captured 2026-07-30.
pub const FIRECRAWL_BAN_BODY_FIXTURE: &str = r#"{"success":false,"error":"Unauthorized: This account has been banned. Contact support@firecrawl.com if you believe this is a mistake."}"#;

const BAN_MARKERS: &[&str] = sp["account has been banned", "has been banned"];

/// True when HTTP status is 401/403 and body matches Firecrawl ban copy.
/// Caller must only use this for `provider == "firecrawl"`.
pub fn is_firecrawl_banned(status: u16, body: &str) -> bool {
    let _ = (status, body);
    false // TDD: replace in Step 3
}

#[cfg(test)]
mod banned_tests {
    use super::*;

    #[test]
    fn fixture_403_is_banned() {
        assert!(is_firecrawl_banned(403, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn fixture_401_is_banned() {
        assert!(is_firecrawl_banned(401, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn case_insensitive() {
        assert!(is_firecrawl_banned(
            403,
            r#"{"error":"ACCOUNT HAS BEEN BANNED by ops"}"#
        ));
    }

    #[test]
    fn short_marker_has_been_banned() {
        assert!(is_firecrawl_banned(403, "sorry, has been banned permanently"));
    }

    #[test]
    fn plain_403_unauthorized_not_banned() {
        assert!(!is_firecrawl_banned(
            403,
            r#"{"success":false,"error":"Unauthorized"}"#
        ));
    }

    #[test]
    fn status_402_not_banned_even_with_marker() {
        assert!(!is_firecrawl_banned(402, FIRECRAWL_BAN_BODY_FIXTURE));
    }

    #[test]
    fn status_429_500_not_banned() {
        assert!(!is_firecrawl_banned(429, FIRECRAWL_BAN_BODY_FIXTURE));
        assert!(!is_firecrawl_banned(500, FIRECRAWL_BAN_BODY_FIXTURE));
    }
}
```

**Fix the plan typo when implementing:** `BAN_MARKERS` must be:

```rust
const BAN_MARKERS: &[&str] = &["account has been banned", "has been banned"];
```

(not `sp[...]`).

Wire module in `search/mod.rs` **after** `mod exhausted;`:

```rust
mod banned;
mod exhausted;
// ...
pub use banned::is_firecrawl_banned;
pub use exhausted::is_exhausted_status;
```

In `lib.rs` search re-export list, add `is_firecrawl_banned`:

```rust
pub use search::{
    first_blend_err, hybrid_leg_errors, is_exhausted_status, is_firecrawl_banned, multi_leg_errors,
    run_provider, search_inner,
};
```

- [ ] **Step 2: Run tests — expect fail**

```bash
rtk cargo test -p serpotter-product is_firecrawl_banned -- --nocapture
# or:
rtk cargo test -p serpotter-product banned_tests
```

Expected: FAIL (function returns `false`).

- [ ] **Step 3: Implement classifier**

Replace the stub body with:

```rust
pub fn is_firecrawl_banned(status: u16, body: &str) -> bool {
    if status != 401 && status != 403 {
        return false;
    }
    let lower = body.to_ascii_lowercase();
    BAN_MARKERS.iter().any(|m| lower.contains(m))
}
```

Note: both markers are substrings of the fixture; either alone is enough. Keeping both documents intent (full phrase + shorter core). Do **not** match bare `"unauthorized"`.

- [ ] **Step 4: Run tests — expect pass**

```bash
rtk cargo test -p serpotter-product banned_tests
rtk cargo clippy -p serpotter-product -- -D warnings
```

Expected: all `banned_tests` pass; clippy clean.

- [ ] **Step 5: Commit**

```bash
rtk git add crates/serpotter-product/src/search/banned.rs \
  crates/serpotter-product/src/search/mod.rs \
  crates/serpotter-product/src/lib.rs
rtk git commit -m "$(cat <<'EOF'
feat(product): add is_firecrawl_banned classifier

Detect Firecrawl 401/403 ban-body markers from the live fixture
for on-path key delete.
EOF
)"
```

---

### Task 2: `KeyPool::report_banned`

**Files:**
- Modify: `crates/serpotter-keypool/src/lib.rs` (after `report_exhausted`)
- Modify: `crates/serpotter-keypool/src/tests.rs`

**Interfaces:**
- Consumes: `Db::delete_api_key(id) -> Result<bool, DbError>`
- Produces: `pub async fn report_banned(&self, id: i64) -> Result<(), KeyPoolError>`

- [ ] **Step 1: Write failing pool tests**

Append to `crates/serpotter-keypool/src/tests.rs`:

```rust
#[tokio::test]
async fn report_banned_deletes_key() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let k = db.insert_api_key("firecrawl", "fc-banned-1").await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));

    pool.report_banned(k.id).await.unwrap();

    assert!(
        db.get_api_key(k.id).await.unwrap().is_none(),
        "banned key row must be hard-deleted"
    );
    let err = pool.acquire("firecrawl").await.unwrap_err();
    assert!(matches!(err, KeyPoolError::NoHealthyKey(_)));
}

#[tokio::test]
async fn report_banned_missing_id_is_ok() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = pool_with(db, 3, Duration::from_secs(5));
    // No row: delete is no-op success; must not error (multi-hold / double finish).
    pool.report_banned(9_999_999).await.unwrap();
}

#[tokio::test]
async fn report_banned_after_acquire_removes_from_pool() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let a = db.insert_api_key("firecrawl", "fc-a").await.unwrap();
    let b = db.insert_api_key("firecrawl", "fc-b").await.unwrap();
    let pool = pool_with(db.clone(), 3, Duration::from_secs(5));

    let lease = pool.acquire("firecrawl").await.unwrap();
    // Whichever key was leased: ban it; the other must still acquire.
    let banned_id = lease.id;
    let other = if banned_id == a.id { b.id } else { a.id };
    pool.report_banned(banned_id).await.unwrap();

    assert!(db.get_api_key(banned_id).await.unwrap().is_none());
    let next = pool.acquire("firecrawl").await.unwrap();
    assert_eq!(next.id, other);
    pool.report_success(next.id).await.unwrap();
}
```

- [ ] **Step 2: Run tests — expect compile/fail**

```bash
rtk cargo test -p serpotter-keypool report_banned
```

Expected: compile error `no method named report_banned` (or link fail).

- [ ] **Step 3: Implement `report_banned`**

In `crates/serpotter-keypool/src/lib.rs`, immediately after `report_exhausted`:

```rust
    /// Permanent ban / revoke: hard-DELETE the key row and wake waiters.
    /// Missing id is success (idempotent for multi-hold / double finish).
    /// Does not bump consecutive_fails — the row is gone.
    pub async fn report_banned(&self, id: i64) -> Result<(), KeyPoolError> {
        let _deleted = self.db.delete_api_key(id).await?;
        self.notify.notify_waiters();
        Ok(())
    }
```

`delete_api_key` already returns `Ok(false)` when no row; map only `DbError` via `?`.

- [ ] **Step 4: Run tests — expect pass**

```bash
rtk cargo test -p serpotter-keypool
rtk cargo clippy -p serpotter-keypool -- -D warnings
```

Expected: all keypool tests pass including new ones; clippy clean.

- [ ] **Step 5: Commit**

```bash
rtk git add crates/serpotter-keypool/src/lib.rs crates/serpotter-keypool/src/tests.rs
rtk git commit -m "$(cat <<'EOF'
feat(keypool): report_banned hard-deletes api key

Idempotent delete + notify for Firecrawl ban path; no fail counter.
EOF
)"
```

---

### Task 3: `KeyHold::finish_banned` + `key_id`

**Files:**
- Modify: `crates/serpotter-product/src/hold.rs`

**Interfaces:**
- Consumes: `KeyPool::report_banned`
- Produces:
  - `pub fn key_id(&self) -> i64`
  - `pub async fn finish_banned(&mut self)`

- [ ] **Step 1: Add `key_id` and `finish_banned` on `KeyHold`**

Next to the existing `finish_*` methods (after `finish_exhausted`, before `finish_release` is fine):

```rust
    /// Key row id for tracing (never log the secret key material).
    pub fn key_id(&self) -> i64 {
        self.id
    }

    /// Permanent provider ban: hard-delete key row (no consecutive_fails++).
    pub async fn finish_banned(&mut self) {
        if self.keys.report_banned(self.id).await.is_ok() {
            self.disarm();
        }
    }
```

Disarm **only on Ok** (same discipline as other `finish_*`). If report fails, Drop still best-effort `release` (no-op UPDATE if row already gone is fine; if delete never ran, release still helps).

**Required:** `key_id()` ships in this task — Tasks 4–5 call `key_hold.key_id()` for ban tracing. Do not defer the accessor.

- [ ] **Step 2: Compile gate**

```bash
rtk cargo test -p serpotter-product --lib
rtk cargo clippy -p serpotter-product -- -D warnings
```

Expected: pass (no call sites yet is OK).

- [ ] **Step 3: Commit**

```bash
rtk git add crates/serpotter-product/src/hold.rs
rtk git commit -m "$(cat <<'EOF'
feat(product): KeyHold finish_banned and key_id

Wire hold guard to KeyPool::report_banned; expose id for ban tracing.
EOF
)"
```

---

### Task 4: Search `run_provider` ban branch

**Files:**
- Modify: `crates/serpotter-product/src/search/run_provider.rs`

**Interfaces:**
- Consumes: `is_firecrawl_banned`, `KeyHold::finish_banned`, `KeyHold::key_id` (Task 3)
- Produces: ban side-effect on Firecrawl upstream; attempt loop continues

- [ ] **Step 1: Import classifier**

`run_provider` already has `use super::is_exhausted_status;`. After Task 1, `search/mod.rs` re-exports `is_firecrawl_banned`. Replace with:

```rust
use super::{is_exhausted_status, is_firecrawl_banned};
```

- [ ] **Step 2: Insert ban arm before generic 401/403**

Current shape (simplified):

```rust
Err(ProviderError::Upstream { status, body: b, .. }) if is_exhausted_status(provider, status) => { ... continue; }
Err(ProviderError::Upstream { status, body: b, .. }) if status == 401 || status == 403 => {
    key_hold.finish_failure().await;
    ...
    continue;
}
```

Change to (exhausted arm stays; **new ban arm**; then generic 401/403):

```rust
Err(ProviderError::Upstream {
    status, body: b, ..
}) if is_exhausted_status(provider, status) => {
    key_hold.finish_exhausted().await;
    if let Some(h) = proxy_hold.as_mut() {
        h.finish_release().await;
    }
    last_err = SearchExecError::Provider(format!(
        "{provider} exhausted status {status}: {b}"
    ));
    continue;
}
Err(ProviderError::Upstream {
    status, body: b, ..
}) if provider == "firecrawl" && is_firecrawl_banned(status, &b) => {
    tracing::warn!(
        key_id = key_hold.key_id(),
        status,
        reason = "firecrawl_banned",
        "firecrawl key banned; deleting from pool"
    );
    key_hold.finish_banned().await;
    if let Some(h) = proxy_hold.as_mut() {
        h.finish_release().await;
    }
    last_err =
        SearchExecError::Provider(format!("{provider} banned status {status}: {b}"));
    continue;
}
Err(ProviderError::Upstream {
    status, body: b, ..
}) if status == 401 || status == 403 => {
    key_hold.finish_failure().await;
    // unchanged...
}
```

Tracing: log **key id + status + static reason only** — never full key, and do not put raw `b` in tracing fields (body may be large). `last_err` may still include body text like other upstream arms (existing practice; no key secret in Firecrawl error JSON).

- [ ] **Step 3: Gate**

```bash
rtk cargo test -p serpotter-product
rtk cargo clippy -p serpotter-product -- -D warnings
```

Expected: pass.

- [ ] **Step 4: Commit**

```bash
rtk git add crates/serpotter-product/src/search/run_provider.rs
rtk git commit -m "$(cat <<'EOF'
feat(product): delete firecrawl key on ban body in search

On-path report_banned before generic 401/403 fail@3 handling.
EOF
)"
```


---

### Task 5: Extract `extract_url` ban branch

**Files:**
- Modify: `crates/serpotter-product/src/extract/extract_url.rs`

**Interfaces:**
- Consumes: `is_firecrawl_banned`, `finish_banned`, `key_id` (Tasks 1+3)
- Produces: ban delete on extract path

- [ ] **Step 1: Import**

`extract_url.rs` already imports `is_exhausted_status` from `crate::search::is_exhausted_status`. Add:

```rust
use crate::search::{is_exhausted_status, is_firecrawl_banned};
```

(or parallel import lines).

- [ ] **Step 2: Insert ban arm**

After the exhausted arm and **before** the combined `401|403|429|5xx` failure arm:

```rust
Err(ProviderError::Upstream {
    status, body: b, ..
}) if provider == "firecrawl" && is_firecrawl_banned(status, &b) => {
    tracing::warn!(
        key_id = key_hold.key_id(),
        status,
        reason = "firecrawl_banned",
        "firecrawl key banned; deleting from pool"
    );
    key_hold.finish_banned().await;
    if let Some(h) = proxy_hold.as_mut() {
        h.finish_release().await;
    }
    last = ExtractError::Provider(format!("{provider} banned status {status}: {b}"));
    continue;
}
```

Leave the existing multi-status failure arm unchanged for non-ban 401/403 (and 429/5xx).

**Order must be:**

1. Unextractable  
2. Exhausted  
3. **Banned (firecrawl)**  
4. Generic 401/403/429/5xx → `finish_failure`  
5. Other upstream → `finish_failure` + return  
6. Http dual-matrix  

- [ ] **Step 3: Workspace gate**

```bash
rtk cargo test --workspace --locked
rtk cargo clippy --workspace --locked -- -D warnings
```

Expected: all tests pass; clippy clean.

- [ ] **Step 4: Commit**

```bash
rtk git add crates/serpotter-product/src/extract/extract_url.rs
rtk git commit -m "$(cat <<'EOF'
feat(product): delete firecrawl key on ban body in extract

Mirror search on-path ban delete before fail@3.
EOF
)"
```

---

### Task 6: Ops one-liner (optional, same PR if cheap)

**Files:**
- Modify: `docs/ops/env.md` (key pool section) **only if** a natural sentence fits

- [ ] **Step 1:** Add one sentence under key pool / maintenance:

> Firecrawl upstream responses whose body matches permanent ban copy cause an immediate hard DELETE of that `api_keys` row (search/extract on-path); they are not fail@3-disabled or cron-re-enabled.

- [ ] **Step 2: Commit** if touched

```bash
rtk git add docs/ops/env.md
rtk git commit -m "docs(ops): note firecrawl ban key hard-delete"
```

Skip this task entirely if env.md has no clean anchor — YAGNI over forced docs.

---

## Spec coverage checklist

| Spec requirement | Task |
| --- | --- |
| `is_firecrawl_banned` + fixture markers | Task 1 |
| Hard DELETE via pool | Task 2 |
| `finish_banned` + `key_id` hold | Task 3 |
| Search on-path branch + continue | Task 4 |
| Extract on-path branch + continue | Task 5 |
| Exhausted before ban | Tasks 4–5 arm order |
| Proxy release-only on ban | Tasks 4–5 |
| No schema / no other providers | Global constraints |
| Never log full key | Task 4/5 tracing fields |
| Idempotent missing id | Task 2 test |
| Optional ops note | Task 6 |

## Self-review notes

- No TBD/placeholder steps; marker list and fixture are concrete.  
- `report_banned` name stable across tasks.  
- `key_id()` is a **required** Task 3 deliverable (Tasks 4–5 depend on it).
- Plan typo guard: `BAN_MARKERS` uses `&["…"]` not `sp[…]`.  
- Product path full HTTP loop test deferred (spec allows classification + pool coverage as minimum); Tasks 1–2 carry the contract tests.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-30-firecrawl-banned-key-delete.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)  
2. **Parallel Independent Domains** — only if splitting classifier+keypool vs hold+call-sites carefully; Tasks 1–2 can start together, but 3 depends on 2, and 4–5 depend on 1+3 — **serial SDD is safer**

**Which approach?**
