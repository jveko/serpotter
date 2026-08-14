# Request Events: Retire request_log, Logs + Ring + Rollup

**Date:** 2026-08-14
**Status:** Approved for implementation planning
**Scope:** Replace the fused `request_log` SQLite table with a split model — structured JSON log events (durable audit), in-memory ring buffer (admin browsing), write-time `usage_daily` rollup (spend/usage), in-memory error window (alerting). Schema **v17**.
**Approach:** A — logs are the audit; SQLite keeps only aggregates.

## Problem

`request_log` is a SQLite table doing three jobs at once (durable audit trail, spend/usage source, metrics feed). That fusion is the source of its problems:

1. **Fragile durability.** `spawn_log` is fire-and-forget: (a) graceful shutdown never awaits in-flight inserts, so the runtime teardown drops them; (b) insert errors become a `warn!` and the row vanishes with no accounting, no retry; (c) a crash (SIGKILL/OOM/power) loses every uncommitted row. The audit is only as durable as a best-effort DB write.
2. **Schema churn.** ~20 nullable columns, 6 indexes, a migration for every new field (v12 columns, v15 token/cost, v16 drop), plus a 30-day/100k-row purge job to keep the OLTP DB bounded.
3. **No producer for usage.** `usage_daily`, `/api/usage`, and `/api/spend/*` exist, but nothing calls `rollup_usage_from_request_log` — the rollup is never run, so the spend surface reads an empty table.
4. **Wrong tool for the job.** Raw per-request events are log-shaped data, not relational data; storing them in the OLTP DB is what forces the columns, indexes, migrations, and purge.

## Goals

1. **Durable, schema-free audit.** Every request emits one structured JSON log line (`LOG_FORMAT=json`) to stdout; Docker/journald own retention and rotation. Loss window on any crash = the log pipeline's, not a DB buffer's.
2. **Zero-loss accounting.** No silent drops anywhere: a full usage channel or failed upsert logs `error!` and increments a counter. The audit line always lands regardless.
3. **Preserve the three surviving surfaces** at identical wire contracts:
   - Admin per-request browsing (`GET /api/request-logs`, same filters/JSON) — now backed by an in-memory ring.
   - Spend/usage analytics (`/api/usage`, `/api/spend/keys`, `/api/spend/services`) — now backed by write-time `usage_daily` upserts (fixing the missing-producer bug).
   - Error-rate alerting (5-min window, ratio 0.5, min-total 20, optional webhook) — now backed by an in-memory window.
4. **Keep `/metrics`** (counters, duration histogram, in-flight gauge, key-pool depth) fed from the same event funnel.
5. **Delete** the `request_log` table, its indexes, purge cron, and every Db method that serves it.

## Non-goals

- Restart-surviving request browsing (the JSON log stream is the durable record; the ring is a convenience window).
- An external log pipeline (Vector/ClickHouse/Loki) — contradicts the single-VPS personal YAGNI posture; no store exists to reuse.
- Rebuilding history from raw events (raw events are no longer stored; `usage_daily` accumulates at write time).
- Full body capture / encrypted audit.

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Durable audit | `tracing::info!` event, `target: "request"`, one per request, all current fields |
| Admin browsing | In-memory ring buffer, cap **2048**, monotonic `seq` as `id`, mirror of current `list_request_logs` semantics |
| Usage/spend persistence | Widen `usage_daily` with `key_id` / `token_name` (sentinel `0` / `''` — SQLite UNIQUE treats NULLs as distinct, so sentinels are required for conflict-dedupe upsert) |
| Usage write path | Single writer task, bounded `mpsc` (1024), `try_send`, drain-on-shutdown (5s timeout), WAL already enabled |
| Alerting | In-memory per-minute buckets, pruned past 5 min; same constants/webhook |
| Metrics | Kept (`metrics::observe` from the funnel) |
| Schema | `DROP TABLE request_log`; rebuild `usage_daily`; `EXPECTED_SCHEMA_VERSION = 17` |
| Wire contracts | `/api/request-logs` JSON, `/api/usage`, `/api/spend/*`, `/metrics`, alert webhook body — all unchanged except `/api/stats` field `requestLogs` → `recentRequests` |

## Architecture

```text
HTTP handlers (REST search/extract/research, MCP run_tool, F08 failed-auth)
  └─ events::emit(&state, fields, started)          [sync, non-blocking]
       ├─ 1. tracing::info!(target: "request", …)   → stdout JSON logs  (durable audit)
       ├─ 2. ring.push(entry)                       → admin /api/request-logs browser (cap 2048)
       ├─ 3. error_window.record(status)            → cron check_error_rate (5-min window)
       ├─ 4. metrics::observe(status, service, duration, tokens…)  → /metrics
       └─ 5. usage_tx.try_send(delta)               → writer task → Db::upsert_usage_daily
                                                      (full → error! + dropped counter)

cron (15m): re-enable keys/nodes, purge admin_sessions, purge query_cache,
            check_error_rate(&error_window) + webhook, optional credit sync

shutdown: serve drains → drop usage_tx → await writer JoinHandle (5s) → exit
```

### 1. Event funnel — `crates/serpotter-api/src/events.rs` (replaces `log_request.rs`)

`spawn_log` / `spawn_log_db` become one synchronous `emit(&state, fields, started)` with the five side effects above, same call shape, so all existing callsites keep working. `LogFields` remains the event struct (all current columns, including token/cost and `cache_hit`).

The log line carries every current column as a field: `path`, `method`, `status`, `service`, `provider_used`, `duration_ms`, `error_kind`, `query_preview`, `request_id`, `token_name`, `strategy`, `providers_consulted`, `attempt_count`, `key_id`, `node_id`, `input_tokens`, `output_tokens`, `total_tokens`, `cost_est`. Future fields are log-field additions, never migrations.

Usage deltas flow through `tokio::sync::mpsc::Sender<UsageDelta>` (bounded 1024) into one writer task that calls `upsert_usage_daily` per event. A full channel or upsert failure logs `error!` and bumps `serpotter_events_dropped_total{reason="channel_full"|"upsert_failed"}` on the metrics registry.

F08 failed-auth events keep landing (status 401, no token name, no usage — the request never reached a provider).

### 2. Ring buffer

`AppState.events: RequestEvents` holds `Mutex<VecDeque<RingEntry>>` (cap 2048) plus a monotonic `seq: u64` used as `id`. `list(filter)` mirrors the current `RequestLogFilter` semantics exactly:

- newest-first; `limit` (clamp 1..=200) + `offset`
- lenient `status` parse (non-numeric values like `"2xx"` treated as absent, matching today — dashboards may pass through raw inputs)
- `path` prefix match, `service` / `request_id` / `token_name` exact match

`GET /api/request-logs` query params and JSON shape (`LogOut`) are unchanged; only the backing store changes. Restart clears the ring — accepted (logs are the record).

### 3. Error window

`ErrorWindow` keeps `Mutex<VecDeque<(i64 epoch_minute, i64 total, i64 errors)>>`; `record(status)` buckets by current minute (2xx = success, else error — same class semantics as metrics), prunes buckets older than `ALERT_WINDOW_MINUTES` (5). `check_error_rate` reads the window: `(total, errors)` over the window; `None` when below `ALERT_MIN_TOTAL` (20) or ratio ≤ 0.5. Same `ErrorRateStats`, same `fire_alert` webhook (`{errorRate, total, errors, ts}`), same constants. After a restart the window is empty, so no alert can fire until 20 requests accumulate.

### 4. Database — migration `0017_request_events.sql` (schema **17**)

```sql
DROP TABLE request_log;               -- 6 indexes drop with it (SQLite)

-- Rebuild usage_daily with key/token dims; PK change requires table rebuild.
CREATE TABLE usage_daily_new (
    service TEXT NOT NULL,
    provider_used TEXT NOT NULL,
    date TEXT NOT NULL,
    key_id INTEGER NOT NULL DEFAULT 0,      -- sentinel: unknown key
    token_name TEXT NOT NULL DEFAULT '',    -- sentinel: unknown token
    requests INTEGER NOT NULL DEFAULT 0,
    successes INTEGER NOT NULL DEFAULT 0,
    errors INTEGER NOT NULL DEFAULT 0,
    tokens INTEGER NOT NULL DEFAULT 0,
    cost REAL NOT NULL DEFAULT 0,
    PRIMARY KEY (service, provider_used, date, key_id, token_name)
);
INSERT INTO usage_daily_new (service, provider_used, date, requests, successes, errors, tokens, cost)
    SELECT service, provider_used, date, requests, successes, errors, tokens, cost
    FROM usage_daily;
DROP TABLE usage_daily;
ALTER TABLE usage_daily_new RENAME TO usage_daily;
CREATE INDEX idx_usage_daily_date ON usage_daily(date);

UPDATE schema_version SET version = 17 WHERE id = 1;
```

Db changes:

- **Removed:** `insert_request_log`, `insert_request_log_full`, `purge_request_log`, `count_request_logs`, `list_request_logs`, `rollup_usage_from_request_log`, `RequestLogRow`, `RequestLogFilter`.
- **Changed:** `upsert_usage_daily` gains `key_id: i64` / `token_name: &str` (sentinel `0`/`''` from the event path) with the new conflict target; `usage_summary` aggregates across the key/token dims (`SUM(...) GROUP BY service, provider_used, date`); `spend_by_key` / `spend_by_service` read from `usage_daily` (`GROUP BY key_id, token_name` / `GROUP BY service`, `SUM(cost)`) with `key_id != 0` for the key view.

Cron loses `purge_request_log`; envs `REQUEST_LOG_RETENTION_DAYS` and `REQUEST_LOG_MAX_ROWS` are removed (docs + `.env.example`).

### 5. Shutdown

After `axum::serve` completes inside the existing two-stage drain, drop the usage sender and await the writer `JoinHandle` with a 5s timeout (`warn!` on timeout). Graceful restarts flush pending rollup deltas; hard kills lose ≤ the in-channel buffer, which only undercounts a daily cell while the audit line survives in the logs.

### 6. Admin SPA

- `LogsPanel`: wire contract unchanged; add a note that it shows the recent in-memory window and that full history lives in the server JSON logs (`LOG_FORMAT=json`).
- `StatsPanel` + `StatsDto`: `requestLogs` → `recentRequests` (ring length; 0 after restart).

### 7. Tests

- Tests that drive real HTTP and assert status/errorKind/requestId/tokenName via `/api/request-logs` keep working unchanged — the real handlers flow through `emit` → ring.
- `admin_logs_pagination.rs`: reseed through the ring (drive real requests or a `#[doc(hidden)]` pub ring push helper) instead of `db.insert_request_log*`.
- `admin_usage.rs`: seed via `upsert_usage_daily` (or the event path) and assert `/api/usage` + `/api/spend/*` — becomes a true producer test, no rollup step.
- `cron.rs` alert tests: push into `ErrorWindow`, assert `check_error_rate` on the window.
- `serpotter-db` unit tests for removed methods are deleted; `usage.rs` tests updated for the new signature + spend/aggregation queries.
- Ring unit tests: cap eviction, filters, pagination, seq monotonicity.

## Docs

- `AGENTS.md`: schema v17, events model, removed envs, SPA stats field rename.
- `docs/ops/env.md` / `.env.example`: drop `REQUEST_LOG_*`.
- `docs/ops/api.md`: `/api/request-logs` now reads the in-memory window; `/api/stats` field rename.
