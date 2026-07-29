# Nodes Protocol + Pool Simplify Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Per-node `http`/`https`/`socks5` protocol, nodes-only `ProxyPool` (drop Fixed env), schema v11, admin SPA/API + ops honesty — keep fail@3 health.

**Architecture:** Additive `nodes.protocol` (DEFAULT `http`). `proxy_url_from_node(protocol, …)` builds scheme URLs. `ProxyPool` always holds `Db`; `ProxyLease.node_id: i64`. Boot ignores `OUTBOUND_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`. Reqwest gains `socks` feature. Clean cutover — no env seed, no PATCH.

**Tech Stack:** Rust crates `serpotter-db`, `serpotter-outbound`, `serpotter-api`, `serpotter-product` (tests only), `serpotter-providers` (reqwest feature); Admin SPA Vite+ TS; sqlx migrations.

**Spec:** `docs/superpowers/specs/2026-07-30-nodes-protocol-pool-simplify-design.md`

## Global Constraints

- Protocols allowlist only: **`http` | `https` | `socks5`** (lowercase wire/storage)
- Schema **v11** via `0011_node_protocol.sql`; `EXPECTED_SCHEMA_VERSION = 11`
- **Drop Fixed** entirely — no `Mode::Fixed`, no env proxy constructor args
- `ProxyLease.node_id: i64` (not `Option`)
- Health: keep `MAX_CONSECUTIVE_FAILURES = 3` tunnel-only auto-disable
- No custom CONNECT dialer — `reqwest::Proxy::all` only
- No admin PATCH; no boot-seed from env URL; no product REST/MCP wire change
- xAI still never acquires outbound
- Never `git commit --no-verify`; conventional commits; prefer `rtk cargo test` / `rtk cargo clippy`
- **Blast radius:** every `insert_node` call site, every node SELECT/RETURNING list (esp. acquire), every `ProxyPool::from_env_and_db` / `with_options(env, …)`, every `node_id: Option` assertion — half-migrate = compile fail or silent `http` forever

## File map

| File | Responsibility |
| --- | --- |
| `crates/serpotter-db/migrations/0011_node_protocol.sql` | ADD `protocol`, schema_version=11 |
| `crates/serpotter-db/src/lib.rs` | `EXPECTED_SCHEMA_VERSION = 11`; optional `is_allowed_node_protocol` |
| `crates/serpotter-db/src/nodes.rs` | `NodeRow.protocol`; all SELECT/RETURNING; `insert_node(..., protocol)` |
| `crates/serpotter-db/tests/migrate.rs` | version 11; all `insert_node` +5th arg |
| `crates/serpotter-outbound/src/lib.rs` | URL builder + nodes-only `ProxyPool` |
| `crates/serpotter-outbound/src/tests.rs` | Drop Fixed tests; protocol/acquire coverage |
| `crates/serpotter-outbound/AGENTS.md` | Nodes-only SoT |
| `Cargo.toml` (workspace) | reqwest feature `socks` |
| `crates/serpotter-api/src/main.rs` | Boot: no env proxy; `ProxyPool::with_options(db, require)` |
| `crates/serpotter-api/tests/common/mod.rs` | `ProxyPool::new(db)` |
| `crates/serpotter-api/src/admin/nodes.rs` | Create/list protocol |
| `crates/serpotter-api/tests/admin_nodes_logs.rs` | `insert_node` + protocol asserts if any |
| `crates/serpotter-product/src/report.rs` | Pool ctor + `node_id` asserts in tests |
| `crates/serpotter-product/src/error.rs` | Doc comments drop “Fixed” |
| `apps/admin/src/features/nodes/{types,queries,NodesPanel}.tsx` | Protocol UI |
| `docs/ops/{env,api,deploy}.md`, `.env.example`, root/`AGENTS.md` | Honesty + v11 |

**`insert_node` call-site inventory (all must gain `protocol`):**

- `crates/serpotter-db/tests/migrate.rs` — ~12 calls
- `crates/serpotter-outbound/src/tests.rs` — ~10 calls
- `crates/serpotter-api/tests/admin_nodes_logs.rs` — 4 calls
- `crates/serpotter-product/src/report.rs` — 2 calls
- `crates/serpotter-api/src/admin/nodes.rs` — 1 production call

**`ProxyPool` constructor inventory:**

- `main.rs` → `with_options(db, require_proxy)`
- `tests/common/mod.rs` → `ProxyPool::new(db)` (or `with_options(db, false)`)
- `outbound/tests.rs` — all
- `product/report.rs` tests — 2

**Node SQL column lists (all must include `protocol`):**

- `insert_node` INSERT + RETURNING
- `list_nodes` SELECT
- `get_node` SELECT
- `acquire_outbound_node_with_ttl` RETURNING (**critical** — pool builds URL from this row)

---

### Task 1: Schema v11 + `NodeRow.protocol` + `insert_node`

**Files:**
- Create: `crates/serpotter-db/migrations/0011_node_protocol.sql`
- Modify: `crates/serpotter-db/src/lib.rs`
- Modify: `crates/serpotter-db/src/nodes.rs`
- Modify: `crates/serpotter-db/tests/migrate.rs` (version assert + every `insert_node`)
- Modify (mechanical 5th arg `"http"` only this task if compile requires): outbound/api/product test call sites listed above — **prefer finishing all call sites in this task** so workspace compiles before Task 2

**Interfaces:**
- Consumes: existing `Db` / sqlx migrations
- Produces:
  - `EXPECTED_SCHEMA_VERSION: i64 = 11`
  - `pub fn is_allowed_node_protocol(p: &str) -> bool` in `serpotter-db` (or `nodes` module re-export)
  - `NodeRow { …, protocol: String, … }`
  - `pub async fn insert_node(&self, host: &str, port: i64, username: Option<&str>, password: Option<&str>, protocol: &str) -> Result<NodeRow, DbError>`

- [ ] **Step 1: Migration file**

Create `crates/serpotter-db/migrations/0011_node_protocol.sql`:

```sql
-- Per-node proxy scheme for reqwest::Proxy::all (http|https|socks5).
ALTER TABLE nodes ADD COLUMN protocol TEXT NOT NULL DEFAULT 'http';

UPDATE schema_version SET version = 11 WHERE id = 1;
```

- [ ] **Step 2: Bump constant + allowlist helper**

In `crates/serpotter-db/src/lib.rs`:

```rust
pub const EXPECTED_SCHEMA_VERSION: i64 = 11;
```

Add (near other pub consts or in `nodes.rs` + `pub use`):

```rust
/// Wire/storage allowlist for `nodes.protocol`.
pub fn is_allowed_node_protocol(protocol: &str) -> bool {
    matches!(protocol, "http" | "https" | "socks5")
}
```

Export from crate root if defined in `nodes.rs`.

- [ ] **Step 3: Failing / compile-break then implement `NodeRow` + SQL**

Update `NodeRow`:

```rust
pub struct NodeRow {
    pub id: i64,
    pub host: String,
    pub port: i64,
    pub protocol: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub enabled: i64,
    pub inflight: i64,
    pub consecutive_fails: i64,
    pub last_error: Option<String>,
    pub lease_until: Option<String>,
}
```

`map_node_row`: `protocol: r.try_get("protocol")?`.

**Column list constant** (optional DRY) — every query uses the same trailing list:

```text
id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until
```

`insert_node`:

```rust
pub async fn insert_node(
    &self,
    host: &str,
    port: i64,
    username: Option<&str>,
    password: Option<&str>,
    protocol: &str,
) -> Result<NodeRow, DbError> {
    if !crate::is_allowed_node_protocol(protocol) {
        return Err(DbError::/* use existing variant or map — if no Validation variant, store only after admin check and use debug_assert / still prefer a real Err */);
    }
    let result = sqlx::query(
        "INSERT INTO nodes (host, port, username, password, protocol) VALUES (?, ?, ?, ?, ?) \
         RETURNING id, host, port, protocol, username, password, enabled, inflight, consecutive_fails, last_error, lease_until",
    )
    .bind(host)
    .bind(port)
    .bind(username)
    .bind(password)
    .bind(protocol)
    .fetch_one(&self.pool)
    .await?;
    map_node_row(&result)
}
```

**If `DbError` has no clean validation variant:** keep allowlist check in admin only for v1, and `insert_node` still binds `protocol` without Err — but **still document** allowlist. Prefer adding a simple `#[error("invalid node protocol: {0}")] InvalidNodeProtocol(String)` on `DbError` if the enum is thiserror and easy to extend; update any exhaustive matches.

Update `list_nodes`, `get_node`, and **`acquire_outbound_node_with_ttl` RETURNING** to include `protocol`.

- [ ] **Step 4: Fix every `insert_node` call site**

Pattern:

```rust
// before
db.insert_node("host", 8080, None, None).await?
// after
db.insert_node("host", 8080, None, None, "http").await?
```

With auth:

```rust
db.insert_node("proxy.example", 8080, Some("u"), Some("p"), "http")
```

Grep until zero 4-arg call sites:

```bash
rtk grep -n "insert_node\(" crates
```

- [ ] **Step 5: migrate test + protocol round-trip test**

In `migrate.rs`:

```rust
assert_eq!(v, serpotter_db::EXPECTED_SCHEMA_VERSION);
assert_eq!(v, 11);
```

Add test (same file or `nodes` unit if present):

```rust
#[tokio::test]
async fn insert_node_protocol_round_trip() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    for proto in ["http", "https", "socks5"] {
        let n = db
            .insert_node(&format!("{proto}.example"), 1, None, None, proto)
            .await
            .unwrap();
        assert_eq!(n.protocol, proto);
        let got = db.get_node(n.id).await.unwrap().unwrap();
        assert_eq!(got.protocol, proto);
    }
    let acq = db.acquire_outbound_node().await.unwrap().unwrap();
    assert!(
        matches!(acq.protocol.as_str(), "http" | "https" | "socks5"),
        "acquire RETURNING must include protocol"
    );
}
```

Optional: default migration path — insert without relying on omitted column (API always passes protocol); existing DBs after migrate read DEFAULT `http`.

- [ ] **Step 6: Run tests**

```bash
rtk cargo test -p serpotter-db
rtk cargo clippy -p serpotter-db -- -D warnings
```

Expected: pass; workspace may still fail until Task 3 if outbound still old URL signature — if you only changed insert arity, outbound/product must already have 5th arg from Step 4.

- [ ] **Step 7: Commit**

```bash
rtk git add crates/serpotter-db
# include mechanical insert_node 5th-arg fixes in other crates if done here
rtk git add -u crates/serpotter-outbound crates/serpotter-api crates/serpotter-product
rtk git commit -m "$(cat <<'EOF'
feat(db): nodes.protocol column and schema v11

Additive protocol (http|https|socks5, default http); insert/list/get/acquire
RETURNING carry protocol; EXPECTED_SCHEMA_VERSION=11.
EOF
)"
```

---

### Task 2: `proxy_url_from_node` protocol + reqwest `socks`

**Files:**
- Modify: `crates/serpotter-outbound/src/lib.rs` (`proxy_url_from_node` only in this task if pool still old — or combine with Task 3 if cleaner)
- Modify: `crates/serpotter-outbound/src/tests.rs` (URL unit tests)
- Modify: root `Cargo.toml` workspace `reqwest` features

**Interfaces:**
- Consumes: `protocol: &str` from row
- Produces: `pub fn proxy_url_from_node(protocol: &str, host: &str, port: u16, username: Option<&str>, password: Option<&str>) -> String`

**Note:** Prefer implementing URL builder + pool rewrite in **one compile cycle** with Task 3 if splitting causes intermediate red workspace. If split: temporary call `proxy_url_from_node("http", …)` from acquire until Task 3 passes row.protocol.

- [ ] **Step 1: Failing URL tests**

Replace/extend URL tests in `crates/serpotter-outbound/src/tests.rs`:

```rust
#[test]
fn proxy_url_http_with_auth() {
    assert_eq!(
        proxy_url_from_node("http", "proxy.example", 8080, Some("u"), Some("p")),
        "http://u:p@proxy.example:8080"
    );
}

#[test]
fn proxy_url_https_and_socks5() {
    assert_eq!(
        proxy_url_from_node("https", "h.example", 443, None, None),
        "https://h.example:443"
    );
    assert_eq!(
        proxy_url_from_node("socks5", "s.example", 1080, Some("u"), Some("p")),
        "socks5://u:p@s.example:1080"
    );
}

#[test]
fn proxy_url_user_only_and_encoding() {
    assert_eq!(
        proxy_url_from_node("http", "h", 1, Some("a@b"), None),
        "http://a%40b@h:1"
    );
}
```

- [ ] **Step 2: Implement builder**

```rust
/// Build `{protocol}://[user:pass@]host:port` for `reqwest::Proxy::all`.
/// `protocol` must already be allowlisted (`http`|`https`|`socks5`).
pub fn proxy_url_from_node(
    protocol: &str,
    host: &str,
    port: u16,
    username: Option<&str>,
    password: Option<&str>,
) -> String {
    debug_assert!(
        serpotter_db::is_allowed_node_protocol(protocol),
        "protocol must be http|https|socks5"
    );
    match (username, password) {
        (Some(u), Some(p)) if !u.is_empty() => {
            format!(
                "{protocol}://{}:{}@{host}:{port}",
                encode_userinfo(u),
                encode_userinfo(p),
            )
        }
        (Some(u), _) if !u.is_empty() => {
            format!("{protocol}://{}@{host}:{port}", encode_userinfo(u))
        }
        _ => format!("{protocol}://{host}:{port}"),
    }
}
```

- [ ] **Step 3: Workspace reqwest socks**

Root `Cargo.toml`:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "socks"] }
```

No providers code change required if `Proxy::all` already takes the URL string.

- [ ] **Step 4: Test URL unit + clippy outbound (may need Task 3 for full package)**

```bash
rtk cargo test -p serpotter-outbound proxy_url
```

- [ ] **Step 5: Commit** (or fold commit with Task 3)

```bash
rtk git add Cargo.toml Cargo.lock crates/serpotter-outbound/src/lib.rs crates/serpotter-outbound/src/tests.rs
rtk git commit -m "$(cat <<'EOF'
feat(outbound): protocol-aware proxy_url_from_node and reqwest socks

Build http|https|socks5 URLs; enable reqwest socks for Proxy::all.
EOF
)"
```

---

### Task 3: Nodes-only `ProxyPool` + `ProxyLease.node_id: i64`

**Files:**
- Modify: `crates/serpotter-outbound/src/lib.rs` (full pool rewrite)
- Rewrite: `crates/serpotter-outbound/src/tests.rs` (delete Fixed tests)
- Modify: `crates/serpotter-api/src/main.rs`
- Modify: `crates/serpotter-api/tests/common/mod.rs`
- Modify: `crates/serpotter-product/src/report.rs` (test ctors + asserts)
- Modify: `crates/serpotter-product/src/error.rs` (doc only)
- Modify: `crates/serpotter-outbound/AGENTS.md`

**Interfaces:**
- Produces:
  - `pub struct ProxyLease { pub node_id: i64, pub url: String }`
  - `pub fn new(db: Db) -> Self` → `with_options(db, false)`
  - `pub fn with_options(db: Db, require_proxy: bool) -> Self`
  - `pub fn with_options_and_hold_ttl(db: Db, require_proxy: bool, hold_ttl_secs: i64) -> Self`
  - **Delete** `from_env_and_db`, env_proxy parameters, `Mode` enum

- [ ] **Step 1: Rewrite `ProxyPool` struct and constructors**

Target shape:

```rust
pub struct ProxyPool {
    db: Db,
    lock: Mutex<()>,
    require_proxy: bool,
    hold_ttl_secs: i64,
}

impl ProxyPool {
    pub fn new(db: Db) -> Self {
        Self::with_options(db, false)
    }

    pub fn with_options(db: Db, require_proxy: bool) -> Self {
        let hold_ttl = std::env::var("NODE_HOLD_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(serpotter_db::NODE_HOLD_TTL_SECS)
            .max(1);
        Self::with_options_and_hold_ttl(db, require_proxy, hold_ttl)
    }

    pub fn with_options_and_hold_ttl(
        db: Db,
        require_proxy: bool,
        hold_ttl_secs: i64,
    ) -> Self {
        Self {
            db,
            lock: Mutex::new(()),
            require_proxy,
            hold_ttl_secs: hold_ttl_secs.max(1),
        }
    }

    pub fn require_proxy(&self) -> bool {
        self.require_proxy
    }

    pub async fn acquire(&self) -> Result<Option<ProxyLease>, ProxyPoolError> {
        let _guard = self.lock.lock().await;
        match self
            .db
            .acquire_outbound_node_with_ttl(self.hold_ttl_secs)
            .await?
        {
            Some(row) => {
                let url = proxy_url_from_node(
                    &row.protocol,
                    &row.host,
                    row.port as u16,
                    row.username.as_deref(),
                    row.password.as_deref(),
                );
                Ok(Some(ProxyLease {
                    node_id: row.id,
                    url,
                }))
            }
            None => Ok(None),
        }
    }

    pub async fn report_success(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        self.db.report_node_success(lease.node_id).await?;
        Ok(())
    }

    pub async fn report_failure(
        &self,
        lease: &ProxyLease,
        error: Option<&str>,
    ) -> Result<(), ProxyPoolError> {
        self.db
            .report_node_failure(
                lease.node_id,
                serpotter_db::MAX_CONSECUTIVE_FAILURES,
                error,
            )
            .await?;
        Ok(())
    }

    pub async fn release(&self, lease: &ProxyLease) -> Result<(), ProxyPoolError> {
        self.db.release_node_inflight(lease.node_id).await?;
        Ok(())
    }
}
```

Update module docs: Fixed gone; nodes → direct.

- [ ] **Step 2: Replace outbound tests**

**Delete entirely:**
- `fixed_mode_ignores_nodes`
- `fixed_report_is_noop_on_nodes`
- `whitespace_env_is_not_fixed`

**Keep/adapt:**

```rust
#[tokio::test]
async fn empty_nodes_returns_none_direct() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = ProxyPool::new(db);
    assert!(pool.acquire().await.unwrap().is_none());
    assert!(!pool.require_proxy());
}

#[tokio::test]
async fn require_proxy_flag_preserved_on_empty_nodes() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let pool = ProxyPool::with_options(db, true);
    assert!(pool.require_proxy());
    assert!(pool.acquire().await.unwrap().is_none());
}

#[tokio::test]
async fn release_decrements_inflight() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    let n = db
        .insert_node("rel.example", 8080, None, None, "http")
        .await
        .unwrap();
    let pool = ProxyPool::new(db.clone());
    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.node_id, n.id);
    // … inflight 1 then release → 0 (same as today)
}

#[tokio::test]
async fn report_failure_disables_at_three() { /* node_id == n.id; insert … "http" */ }

#[tokio::test]
async fn acquire_builds_url_from_row_protocol() {
    let db = connect_and_migrate("sqlite::memory:").await.unwrap();
    db.insert_node("proxy.example", 8080, Some("u"), Some("p"), "socks5")
        .await
        .unwrap();
    let pool = ProxyPool::new(db);
    let lease = pool.acquire().await.unwrap().unwrap();
    assert_eq!(lease.url, "socks5://u:p@proxy.example:8080");
    pool.report_success(&lease).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_acquire_least_inflight_distinct() {
    // … insert with "http"
    let pool = Arc::new(ProxyPool::new(db.clone()));
    // ids: use l1.node_id and l2.node_id directly (no .unwrap() on Option)
    let ids: std::collections::HashSet<i64> = [l1.node_id, l2.node_id].into_iter().collect();
    // …
}
```

- [ ] **Step 3: Wire boot + test fixtures**

`main.rs`:

```rust
let require_proxy = matches!(
    env::var("REQUIRE_OUTBOUND_PROXY")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str(),
    "1" | "true" | "yes"
);
let outbound = Arc::new(ProxyPool::with_options(db.clone(), require_proxy));
tracing::info!(
    require_proxy,
    "outbound ProxyPool is nodes-only (xAI always direct; OUTBOUND_PROXY env ignored)"
);
```

**Remove** `env_proxy` / `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` reads for pool construction.

`tests/common/mod.rs`:

```rust
outbound: Arc::new(ProxyPool::new(db.clone())),
```

`product/report.rs` tests:

```rust
let outbound = Arc::new(ProxyPool::new(db.clone()));
// …
assert_eq!(lease_p.node_id, n.id);
```

`error.rs` doc: `/// Fail-closed egress when REQUIRE_OUTBOUND_PROXY and no healthy node lease.`

- [ ] **Step 4: Grep clean**

```bash
rtk grep -n "from_env_and_db|Mode::Fixed|OUTBOUND_PROXY|node_id: None|Some\(n\.id\)|with_options\(None|with_options\(Some|from_env_and_db" crates
```

Expected: no Fixed/env pool constructors; no `Option` node_id pattern left in outbound/product tests (except unrelated Options).

- [ ] **Step 5: Test**

```bash
rtk cargo test -p serpotter-outbound
rtk cargo test -p serpotter-product report
rtk cargo test -p serpotter-api
rtk cargo clippy --workspace -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
rtk git add crates/serpotter-outbound crates/serpotter-api/src/main.rs \
  crates/serpotter-api/tests crates/serpotter-product
rtk git commit -m "$(cat <<'EOF'
feat(outbound): nodes-only ProxyPool drop Fixed env

ProxyLease.node_id is i64; constructors take db only; boot ignores
OUTBOUND_PROXY/HTTPS_PROXY/HTTP_PROXY; acquire uses row.protocol.
EOF
)"
```

---

### Task 4: Admin API protocol

**Files:**
- Modify: `crates/serpotter-api/src/admin/nodes.rs`
- Modify: `crates/serpotter-api/tests/admin_nodes_logs.rs` (asserts + any HTTP create tests)

**Interfaces:**
- `CreateNodeBody { host, port, username?, password?, protocol?: Option<String> }`
- `NodeOut { …, protocol: String, … }`
- Default protocol when absent/blank: **`http`**
- Invalid protocol → **400** `ValidationError`

- [ ] **Step 1: DTOs + create validation**

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeOut {
    id: i64,
    host: String,
    port: i64,
    protocol: String,
    enabled: bool,
    inflight: i64,
    consecutive_fails: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lease_until: Option<String>,
}

fn node_out(r: serpotter_db::NodeRow) -> NodeOut {
    NodeOut {
        id: r.id,
        host: r.host,
        port: r.port,
        protocol: r.protocol,
        enabled: r.enabled != 0,
        inflight: r.inflight,
        consecutive_fails: r.consecutive_fails,
        username: r.username,
        last_error: r.last_error,
        lease_until: r.lease_until,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateNodeBody {
    pub host: String,
    pub port: i64,
    pub username: Option<String>,
    pub password: Option<String>,
    pub protocol: Option<String>,
}
```

In `create_node` after host/port check:

```rust
let protocol = body
    .protocol
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .unwrap_or("http");
if !serpotter_db::is_allowed_node_protocol(protocol) {
    return problem_response(
        StatusCode::BAD_REQUEST,
        "ValidationError",
        "protocol must be http, https, or socks5",
    );
}
// insert_node(..., protocol)
```

- [ ] **Step 2: Integration tests**

Extend `admin_nodes_logs.rs` (or dedicated test):

```rust
#[tokio::test]
async fn create_node_default_protocol_http() {
    // POST /api/nodes { host, port } without protocol → 201, body.protocol == "http"
}

#[tokio::test]
async fn create_node_socks5_ok() {
    // protocol: "socks5" → 201
}

#[tokio::test]
async fn create_node_bad_protocol_400() {
    // protocol: "ftp" → 400 ValidationError
}
```

Use existing admin auth helpers from the suite (`state_with`, admin headers). Match list response includes `protocol`.

Existing `insert_node` setup calls already have `"http"` from Task 1.

- [ ] **Step 3: Run**

```bash
rtk cargo test -p serpotter-api admin_nodes
rtk cargo clippy -p serpotter-api -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
rtk git add crates/serpotter-api/src/admin/nodes.rs crates/serpotter-api/tests
rtk git commit -m "$(cat <<'EOF'
feat(api): node create/list protocol field

Optional CreateNodeBody.protocol default http; allowlist validation;
NodeOut always includes protocol.
EOF
)"
```

---

### Task 5: Admin SPA protocol UI

**Files:**
- Modify: `apps/admin/src/features/nodes/types.ts`
- Modify: `apps/admin/src/features/nodes/queries.ts`
- Modify: `apps/admin/src/features/nodes/NodesPanel.tsx`

- [ ] **Step 1: Types + create request**

`types.ts`:

```ts
export type NodeRow = {
  id: number;
  host: string;
  port: number;
  protocol: string;
  enabled: boolean;
  inflight: number;
  consecutiveFails: number;
  username?: string | null;
  lastError?: string | null;
  leaseUntil?: string | null;
};
```

`queries.ts` — extend `createNodeRequest`:

```ts
export async function createNodeRequest(p: {
  host: string;
  port: number | string;
  protocol?: string;
  username?: string;
  password?: string;
}): Promise<unknown> {
  const body: Record<string, unknown> = {
    host: String(p.host ?? "").trim(),
    port: Number(p.port),
    protocol: (p.protocol ?? "http").trim() || "http",
  };
  const user = p.username != null ? String(p.username).trim() : "";
  if (user) body.username = user;
  if (p.password) body.password = p.password;
  return adminFetch("/api/nodes", {
    method: "POST",
    body: JSON.stringify(body),
  });
}
```

- [ ] **Step 2: NodesPanel form + list + copy**

State:

```ts
const [nodeProtocol, setNodeProtocol] = useState("http");
```

Add form field **before** host:

```tsx
<label className="field">
  <span className="field__label">Protocol</span>
  <select
    className="input"
    value={nodeProtocol}
    onChange={(e) => setNodeProtocol(e.target.value)}
    disabled={busy}
  >
    <option value="http">HTTP</option>
    <option value="https">HTTPS</option>
    <option value="socks5">SOCKS5</option>
  </select>
</label>
```

`handleCreate` / mutation variables include `protocol: nodeProtocol`.

**Lede (add section):** replace Fixed wording, e.g.

```text
HTTP, HTTPS, or SOCKS5 proxies for tavily, firecrawl, and exa. Username and password may be empty.
```

**List section note:**

```text
Per attempt: least-inflight enabled node, else direct (or 503 if REQUIRE_OUTBOUND_PROXY). xAI is always direct. Env OUTBOUND_PROXY is not used.
```

**Table:** add `<th>protocol</th>` after id (or before host); cell `{n.protocol}`; empty `colSpan` **11**.

Filter may also match `n.protocol`.

- [ ] **Step 3: Typecheck**

```bash
cd apps/admin && npm run typecheck
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
rtk git add apps/admin/src/features/nodes
rtk git commit -m "$(cat <<'EOF'
feat(admin): node protocol select and list column

SPA create sends http|https|socks5; honest nodes-only lede; drop Fixed copy.
EOF
)"
```

---

### Task 6: Ops docs + AGENTS honesty

**Files:**
- Modify: `docs/ops/env.md` (Outbound section)
- Modify: `docs/ops/api.md`
- Modify: `docs/ops/deploy.md` (schema **11**)
- Modify: `.env.example`
- Modify: root `AGENTS.md` (schema 10→11, ProxyPool blurb)
- Modify: `crates/serpotter-db/AGENTS.md`, `crates/serpotter-api/AGENTS.md`, `crates/serpotter-outbound/AGENTS.md` if not done
- Optional: `crates/serpotter-product/AGENTS.md` Fixed wording

- [ ] **Step 1: env.md outbound rewrite**

Replace Fixed priority with:

```markdown
## Outbound proxy (web providers only)

`ProxyPool` is **nodes-only**: each non-xAI product attempt acquires the least-inflight **enabled** `nodes` row (or dials direct when none). Reqwest owns the tunnel via `Proxy::all` (HTTP/HTTPS/SOCKS5 URLs from `nodes.protocol`). **No Fixed env mode** — `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` are **ignored** for Serpotter egress (breaking vs pre-v11); put proxies in admin **Nodes**.

| Variable | Default | Notes |
| --- | --- | --- |
| `REQUIRE_OUTBOUND_PROXY` | off | `1`/`true`/`yes` → **503 NoHealthyNode** when no enabled node lease. **xAI still direct**. |
| `NODE_HOLD_TTL_SECS` | `90` | Multi-hold reclaim for `nodes.lease_until`. Boot zeros inflight + lease. |
```

- [ ] **Step 2: api.md / deploy.md**

```markdown
- Proxy: live enabled `nodes` (protocol http|https|socks5) → direct
- Schema readiness: `/ready` needs schema version **≥ 11**
```

`deploy.md`: Schema version **11**; readiness ≥ 11.

- [ ] **Step 3: `.env.example`**

```bash
# Outbound: configure proxies in admin Nodes (http|https|socks5). 
# OUTBOUND_PROXY / HTTPS_PROXY / HTTP_PROXY are ignored by Serpotter (nodes-only pool).
# xAI always dials direct.
# Fail-closed when no enabled node (product → 503 NoHealthyNode):
# REQUIRE_OUTBOUND_PROXY=1
# NODE_HOLD_TTL_SECS=90
```

- [ ] **Step 4: AGENTS blurb**

Root NOTES: `EXPECTED_SCHEMA_VERSION` **11**; v11 `nodes.protocol`; outbound Fixed removed.

- [ ] **Step 5: Commit**

```bash
rtk git add docs/ops .env.example AGENTS.md crates/*/AGENTS.md
rtk git commit -m "$(cat <<'EOF'
docs(ops): nodes-only outbound and schema v11

Remove Fixed env proxy docs; document protocol allowlist and breaking env ignore.
EOF
)"
```

---

### Task 7: Workspace gate

- [ ] **Step 1: Full quality**

```bash
rtk cargo test --workspace
rtk cargo clippy --workspace -- -D warnings
cd apps/admin && npm run typecheck
```

Expected: all green.

- [ ] **Step 2: Final greps**

```bash
rtk grep -n "Mode::Fixed|from_env_and_db|EXPECTED_SCHEMA_VERSION = 10|schema version \*\*10\*\*|Fixed env|hardcodes \`http" crates docs apps AGENTS.md
```

Expected: clean (spec/plan historical mentions OK under `docs/superpowers/`).

- [ ] **Step 3: No extra commit unless fixes** — fix-up commits as needed.

---

## Spec coverage checklist

| Spec item | Task |
| --- | --- |
| `nodes.protocol` + v11 migration | 1 |
| Allowlist http/https/socks5 | 1, 4 |
| `proxy_url_from_node(protocol, …)` | 2 |
| reqwest `socks` | 2 |
| Drop Fixed / constructors | 3 |
| `ProxyLease.node_id: i64` | 3 |
| Boot ignore proxy env | 3 |
| Acquire uses row.protocol | 3 |
| Admin Create/List protocol | 4 |
| SPA select + list + lede | 5 |
| Ops/AGENTS/.env.example | 6 |
| fail@3 unchanged | 3 tests keep disable@3 |
| No product wire change | (no task touches product handlers) |
| All insert/SELECT/ctor blast radius | 1 + 3 file lists |

## Placeholder / consistency self-check

- No TBD steps; signatures use `insert_node(..., protocol: &str)`, `ProxyPool::new(db)` / `with_options(db, require_proxy)`, `node_id: i64`.
- Task 1 explicitly lists every `insert_node` file; Task 3 lists every pool ctor file.
- Acquire RETURNING includes `protocol` in Task 1 (not deferred).

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-30-nodes-protocol-pool-simplify.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`subagent-driven-development`)

**2. Parallel Independent Domains** — only where tasks do not share files (limited here: Tasks 1→3 are serial; 4–5 can follow 3; 6 docs can parallel 5 after API shapes stable)

**Which approach?**
