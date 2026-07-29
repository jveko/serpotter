# serpotter-outbound

**Generated:** 2026-07-29 · twin of keypool (egress)

## OVERVIEW

`ProxyPool` + `proxy_url_from_node`. Builds URL strings for `reqwest::Proxy::all` — **no** CONNECT dialer, **no** reqwest dep. Mode fixed at construction: Fixed env | live nodes | direct.

## STRUCTURE

```
src/
├── lib.rs     # proxy_url_from_node, ProxyLease, ProxyPool
└── tests.rs   # Fixed vs nodes, fail@3, concurrent least-inflight
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| URL from node row | `proxy_url_from_node` |
| Construct pool | `from_env_and_db` / `with_options` / `with_options_and_hold_ttl` |
| Lease one attempt | `ProxyPool::acquire` → `Option<ProxyLease>` |
| Outcome | `report_success` / `report_failure` / `release` |
| Fail-closed flag | `require_proxy()` (product maps bare `None` → `NoHealthyNode`) |
| Hold TTL | `NODE_HOLD_TTL_SECS` (default 90) via db acquire |

## CONVENTIONS

- Non-empty env proxy (`OUTBOUND_PROXY` → `HTTPS_PROXY`/`HTTP_PROXY` at api boot) → **Fixed forever** (db dropped; nodes never touched).
- Else least-inflight enabled `nodes` via `Db::acquire_outbound_node_with_ttl`; empty → `None` (direct) unless product enforces `require_proxy`.
- `ProxyLease { node_id, url }`: Fixed leases use `node_id = None` — report/release no-ops on nodes.
- Single-process only (mutex); not multi-instance safe.
- Concurrent pool tests use **file** SQLite (not `:memory:`) when multi-conn needed.

## ANTI-PATTERNS

- Do not implement a custom CONNECT dialer — only URL + `Proxy::all` downstream.
- Do not touch `nodes` while in Fixed mode.
- Do not report node failure on `node_id = None`.
- Do not claim multi-process lease safety.
- Do not pull reqwest/providers into this crate.
