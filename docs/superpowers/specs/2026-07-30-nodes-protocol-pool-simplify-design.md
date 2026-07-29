# Nodes Protocol + Pool Simplify Design

**Date:** 2026-07-30  
**Status:** Approved for implementation planning  
**Scope:** Add per-node proxy protocol (`http` | `https` | `socks5`), drop Fixed-env `ProxyPool` mode, keep fail@3 health and structured create form

## Problem

Outbound **nodes** are the live commercial-proxy inventory for non-xAI providers, but operators experience them as half-finished:

1. **No protocol field.** Schema is `host` / `port` / `username` / `password` only. `proxy_url_from_node` hardcodes `http://`. SOCKS5 and HTTPS proxy endpoints cannot be represented; SPA and API offer no scheme choice.
2. **Fixed env shadows the table.** Boot non-empty `OUTBOUND_PROXY` → `HTTPS_PROXY` / `HTTP_PROXY` locks `ProxyPool` into `Mode::Fixed` for the process lifetime: nodes CRUD still works in admin, but acquire never touches the table. Mental model: “I added nodes; nothing rotates.”
3. **Honesty / UX debt.** Copy and ops docs still describe Fixed → nodes → direct. Health policy (fail@3 tunnel-only) is fine but is easy to misread next to the Fixed path and missing protocol.

Personal-use constraint: stay **simple** — no custom CONNECT dialer, no multi-instance lease redesign, no edit-node PATCH in v1.

## Goals

1. Operators choose **protocol** at create: `http` | `https` | `socks5`.
2. URL builder emits `{protocol}://[user:pass@]host:port` for `reqwest::Proxy::all`.
3. **Drop Fixed env path** — pool is always live enabled nodes → direct (or 503 when `REQUIRE_OUTBOUND_PROXY`).
4. Keep **N=3** consecutive tunnel failures → auto-disable; success clears fails; provider 4xx/5xx do not blame nodes.
5. Admin SPA + API expose protocol on create and list; docs/env match reality.
6. Existing node rows remain valid (default `http`).

## Non-goals

- Custom CONNECT dialer (still URL string + `reqwest::Proxy::all` only)
- SOCKS4, `socks5h`, or non-userinfo auth
- Boot-seeding a node from env proxy URL (Approach B rejected)
- Keep Fixed mode with documentation-only fix (Approach C rejected)
- Admin **PATCH**/edit node (create + toggle + delete only)
- Change fail threshold, re-enable cron for nodes, or multi-process lease safety
- Intentional product REST/MCP wire shape changes (search/extract/research)

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Approach | **A — clean cutover** |
| Protocols | `http` \| `https` \| `socks5` |
| Create UX | Protocol dropdown + host/port/user/pass (not full-URL paste) |
| Fixed env | **Removed** — do not read `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` for pool mode |
| Health | Keep `MAX_CONSECUTIVE_FAILURES = 3` auto-disable |
| Schema | Additive `nodes.protocol`, bump to **v11** |
| `ProxyLease.node_id` | **`i64`** (always a real node; Fixed gone) |
| Reqwest | Add workspace feature **`socks`** for SOCKS5 URLs |
| xAI | Still always direct (no outbound acquire) |

## Data model

### Migration `0011_node_protocol.sql`

```sql
-- Per-node proxy scheme for Proxy::all URLs (http|https|socks5).
ALTER TABLE nodes ADD COLUMN protocol TEXT NOT NULL DEFAULT 'http';

UPDATE schema_version SET version = 11 WHERE id = 1;
```

- `EXPECTED_SCHEMA_VERSION = 11` in `serpotter-db`.
- Existing rows get `http` via DEFAULT (bit-identical URLs to today’s builder).
- Allowlist enforced in application code on insert (and admin validation). Do **not** rely on SQLite CHECK alone as the only gate, but a CHECK is optional YAGNI — prefer Rust validation + tests.

### `NodeRow`

Add:

```rust
pub protocol: String, // "http" | "https" | "socks5"
```

Update `map_node_row`, `list_nodes` / `get_node` SELECT lists, and `insert_node`:

```text
insert_node(host, port, username, password, protocol) -> NodeRow
```

Unknown protocol at insert → `DbError` or caller validation (admin returns 400 before insert). Prefer **admin validates first**; db may still assert allowlist for defense in depth if cheap.

### URL builder

```text
proxy_url_from_node(protocol, host, port, username?, password?) -> String
```

Rules:

- `protocol` must be one of `http`, `https`, `socks5` (lowercase). Callers never pass free-form schemes from the wire without validation.
- Userinfo encoding unchanged (`%`, space, `@`, `:`).
- Shapes:
  - with user+pass: `{protocol}://{user}:{pass}@{host}:{port}`
  - user only: `{protocol}://{user}@{host}:{port}`
  - neither: `{protocol}://{host}:{port}`

No silent fallback to `http` on bad protocol in production paths.

## ProxyPool architecture

### Before

```text
env non-empty → Mode::Fixed(url)  // drop Db; ignore nodes forever
else          → Mode::Nodes(db)   // least-inflight or None (direct)
```

### After

```text
always Nodes(db): acquire_outbound_node_with_ttl → proxy_url_from_node(protocol, …)
                 or None → direct (unless require_proxy)
```

### API shape

| Item | Change |
| --- | --- |
| `Mode` | Delete enum; hold `Db` directly on `ProxyPool` |
| `from_env_and_db` / `with_options(env_proxy, …)` | Replace with `new(db)` / `with_options(db, require_proxy)` / `with_options_and_hold_ttl(db, require_proxy, hold_ttl_secs)` |
| `ProxyLease` | `node_id: i64`, `url: String` |
| `report_*` / `release` | Always use `lease.node_id` (no `node_id = None` no-op branch for Fixed) |
| `require_proxy` | Unchanged semantics: product maps `acquire → None` to `NoHealthyNode` |
| Hold TTL | Still `NODE_HOLD_TTL_SECS` (default 90) |

### Boot (`serpotter-api` `main.rs`)

- Stop constructing pool from `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`.
- Still read `REQUIRE_OUTBOUND_PROXY` and pass `require_proxy`.
- Still zero node inflight + `lease_until` on boot.
- Log once that outbound is **nodes-only** (optional clarity).

### Providers

- Workspace `reqwest` features: keep `json`, `rustls-tls`; **add `socks`**.
- `try_build_http(Some(url))` unchanged — `Proxy::all` must accept `socks5://` after feature enable.
- xAI clients never receive proxy URL (product does not acquire outbound for xAI).

### Health (unchanged policy)

| Event | Effect |
| --- | --- |
| Success | `consecutive_fails = 0`, clear `last_error`, inflight-- |
| Tunnel / connect class fail | `consecutive_fails++`, store `last_error`, **disable at 3**, inflight-- |
| Provider 4xx/5xx (not tunnel) | release / success-class per existing `classify_proxied_http` — **do not** blame node |
| Admin re-enable | `set_node_enabled(true)` clears fails + last_error (already) |

## Admin API

| Surface | Change |
| --- | --- |
| `NodeOut` | Add `protocol: String` (camelCase JSON key `protocol`) |
| `CreateNodeBody` | Add `protocol: Option<String>` — default **`http`** when absent/null/blank |
| Validation | host non-empty, port > 0, protocol ∈ allowlist → else **400** `ValidationError` |
| Routes | Unchanged: `GET/POST /api/nodes`, `DELETE /api/nodes/{id}`, `POST /api/nodes/{id}/toggle` |
| Password | Still omitted from list/out |

No PATCH in v1: wrong protocol → delete + recreate.

## Admin SPA

| Area | Change |
| --- | --- |
| Types | `NodeRow.protocol: string` |
| Create form | `<select>`: HTTP / HTTPS / SOCKS5 → wire `http` / `https` / `socks5`; default HTTP |
| Create payload | Include `protocol` |
| List | Show protocol (column or combined `protocol://host:port`) |
| Lede | Honest: table is the only proxy inventory; protocols http/https/socks5; empty enabled set → direct unless require-proxy; **no env Fixed override** |
| Toggle / delete | Unchanged |

## Ops & docs

Update in the same implementation wave (behavior + honesty together):

| Doc / file | Change |
| --- | --- |
| `docs/ops/env.md` | Remove Fixed priority tree and Fixed rows for `OUTBOUND_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY`. Document them as **ignored / removed** (one-line breaking note: put proxies in admin nodes). Keep `REQUIRE_OUTBOUND_PROXY`, `NODE_HOLD_TTL_SECS`. |
| `docs/ops/api.md` | Nodes create includes protocol; outbound = live nodes → direct |
| `docs/ops/deploy.md` | Schema **11**; no Fixed wording |
| `.env.example` | Remove or comment-out proxy URL examples as non-functional; point to admin nodes |
| `crates/serpotter-outbound/AGENTS.md` | Nodes-only pool; protocol in URL builder; no Fixed |
| Root / crate `AGENTS.md` as needed | `EXPECTED_SCHEMA_VERSION=11`; outbound blurb |

Compose files: no requirement to inject proxy env; optional comments only.

## Tests

1. **URL builder** — each protocol × (no auth / user / user+pass); encoding still escapes `@` etc.
2. **DB** — insert with each protocol; list/get round-trip; default path for migration (old rows read as `http`).
3. **ProxyPool** — acquire builds URL with row protocol; delete Fixed-ignores-nodes tests; empty nodes → `None`; fail@3 still disables (existing).
4. **Admin API** — create omit protocol → `http`; `socks5` accepted; `ftp` / garbage → 400; list includes protocol.
5. **Migrate** — `EXPECTED_SCHEMA_VERSION == 11`.
6. **Workspace** — clippy/test green; reqwest `socks` feature compiles.
7. **SPA** — `npm run typecheck` (or project equivalent) after type/form updates.

Integration suites that constructed `ProxyPool::with_options(Some(url), …)` must switch to insert-node + nodes-only constructors.

## Error / product surface

- No intentional change to search/extract/research JSON or MCP tool schemas.
- `NoHealthyNode` message: drop any “Fixed” implication; keep fail-closed meaning when `require_proxy` and no enabled node.
- Problem+json for admin validation stays `ValidationError`.

## Rollout

1. Land migration + code + docs; CI green.
2. Deploy image (migrate on boot to v11).
3. Operators: create nodes with correct protocol; remove reliance on process proxy env.
4. Confirm non-xAI traffic rotates least-inflight; SOCKS5 node smoke if inventory has one.

## Open follow-ups (out of v1)

- Admin PATCH to change protocol/host without delete
- Optional one-shot boot seed from legacy env URL
- `socks5h` / DNS-via-proxy semantics
- Node auto re-enable after cooldown (keys-style cron)
- Bulk import from proxy list file

## Implementation next step

After written-spec approval: **writing-plans** → `docs/superpowers/plans/2026-07-30-nodes-protocol-pool-simplify.md`, then implement.
