# serpotter-outbound

**Generated:** 2026-07-30 · twin of keypool (egress)

## OVERVIEW

`ProxyPool` + `proxy_url_from_node`. Builds URL strings for `reqwest::Proxy::all` — **no** CONNECT dialer, **no** reqwest dep in this crate. Nodes-only: least-inflight enabled row or direct (`None`).

## STRUCTURE

```
src/
├── lib.rs     # proxy_url_from_node, ProxyLease, ProxyPool
└── tests.rs   # protocol URLs, fail@3, concurrent least-inflight
```

## WHERE TO LOOK

| Task | Location |
|------|----------|
| URL from node row | `proxy_url_from_node(protocol, host, port, user, pass)` |
| Construct pool | `new` / `with_options` / `with_options_and_hold_ttl` |
| Lease one attempt | `ProxyPool::acquire` → `Option<ProxyLease>` |
| Outcome | `report_success` / `report_failure` / `release` |
| Fail-closed flag | `require_proxy()` (product maps bare `None` → `NoHealthyNode`) |
| Hold TTL | `NODE_HOLD_TTL_SECS` (default 90) via db acquire |

## CONVENTIONS

- Always holds `Db`; no Fixed env mode — boot ignores `OUTBOUND_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`.
- Least-inflight enabled `nodes` via `Db::acquire_outbound_node_with_ttl`; empty → `None` (direct) unless product enforces `require_proxy`.
- `ProxyLease { node_id: i64, url }`: always a real node; report/release always hit DB.
- URL scheme comes from `row.protocol` (`http`|`https`|`socks5`); workspace reqwest has `socks`.
- Single-process only (mutex); not multi-instance safe.
- Concurrent pool tests use **file** SQLite (not `:memory:`) when multi-conn needed.

## ANTI-PATTERNS

- Do not implement a custom CONNECT dialer — only URL + `Proxy::all` downstream.
- Do not reintroduce Fixed env / `from_env_and_db` / `Option` node_id.
- Do not claim multi-process lease safety.
- Do not pull reqwest/providers into this crate.
