# Web Admin Redesign — Design

**Date:** 2026-08-24
**Status:** Approved (user sign-off on all sections)
**Scope:** `apps/admin` SPA → renamed to `web/`, dashboard-first IA, resource UX, Cobalt v2 visual refresh. Zero backend/wire changes.

## Goals

1. Rename the admin SPA from `apps/admin` to `web/` (frontend reads as a frontend, not "apps").
2. Make the landing experience a meaningful operations dashboard built exclusively on existing endpoints.
3. Improve resource-panel UX (keys/nodes/tokens), logs scannability, playground ergonomics.
4. Visual refresh within the existing Cobalt identity ("Cobalt v2") — tighter rhythm, denser tables, refined dark mode.

## Non-goals

- No Rust/backend changes; no new endpoints; no wire-format changes.
- No hourly usage granularity, historical log search past restarts, provider-health timelines, or JSON latency percentiles (scouted gaps — YAGNI for personal scope).
- No chart library dependency (pure SVG).
- No theme toggle (dark mode follows `prefers-color-scheme`).

## Scouting basis

Backend surface inventory and current SPA consumption were mapped by parallel scouts before design:

- **Unused by SPA today:** `GET /api/spend/keys`, `GET /api/spend/services` (per-key/per-service spend leaderboards), admin-gated Prometheus `GET /metrics` (out of scope for this redesign).
- **Under-consumed:** `/api/usage?days=N` time series (rendered as flat table only); `/api/stats.byService` credit pools (thin stat strip only); request-log ring rows with `durationMs` (table only, no aggregation).
- **Constraint honored:** `GET /api/tokens` returns masked previews only (`tokenPreview`); full plaintext exists solely in the create-response.

## 1. Information architecture

```
web/                          ← renamed from apps/admin
Routes (TanStack file-based):
/login                        ← unchanged auth flow
/dashboard                    ← NEW default landing after login
/keys   /nodes   /tokens      ← resources (better tables, inline actions)
/logs                         ← request browser (presets + latency summary)
/playground                   ← product smoke tester
/settings                     ← flag, password, sessions
```

- Sidebar reorders to match; ⌘K gains dashboard actions.
- Deep links: failing key → `/keys?focus=<id>`; error rows → filtered `/logs`; dashboard window → `?days=N`.

## 2. Dashboard composition (`/dashboard`)

Built from four existing query surfaces: `/api/stats`, `/api/usage?days=N`, `/api/spend/*`, `/api/request-logs`.

### KPI strip (4 tiles)

1. **Requests (window)** — sum of `requests` from usage over selected window; delta vs previous equal window.
2. **Error rate** — `errors / requests` over window; amber chip ≥ 10%, red chip ≥ 25%, neutral below.
3. **Spend (window)** — sum of daily `cost`; per-service split beneath.
4. **Pool health** — `activeApiKeys/apiKeys` + enabled node count from stats.

### Usage chart

Stacked bars by service (tavily/firecrawl/exa/xai) over selectable 7/14/30/90d; error overlay as thin red line (`errors` per day). Pure SVG. Window selection lives in URL search param (`?days=14`), validated by TanStack Router.

### Spend leaderboard

Two columns: top 5 keys (`spend/keys`) and all services (`spend/services`) with cost + request share bars.

### Pool health row

Per-service credit bar (`creditsRemaining`/`creditsLimit`; unknown credits = hatched neutral). Failing-key count (`consecutiveFails > 0`) and node issues (`lastError` set or disabled) surfaced as deep-linking chips.

### Recent activity

Last 8 ring events (reuse `requestLogs.list({limit: 8})`): status dot, path, service, `durationMs`, relative time → link to filtered `/logs`.

### Data policy

stats staleTime 10s (unchanged); usage/spend staleTime 60s; activity staleTime 0.

## 3. Resource panels & logs/playground

- **Keys** — inline toggle/rotate (PUT dialog)/delete-with-confirm. Status column: `consecutiveFails` badge + inline credit bar. `?focus=<id>` scrolls + highlights. Sync-credits stays a toolbar action with report toast.
- **Nodes** — inline toggle/test/delete; test result (`latencyMs`/`lastError`) rendered as row status chip instead of toast-only.
- **Tokens** — create flow moves to a proper dialog with copy button for the one-shot plaintext; list shows relative `createdAt`.
- **Logs** — status-class preset chips (`All/2xx/4xx/5xx`) mapping onto existing `status` filter param; computed latency summary header (p50/p95 over currently loaded rows, labeled "ring window"); low-signal columns (`strategy`, `providersConsulted`, `attemptCount`, `keyId`, `nodeId`) collapse into expandable row detail.
- **Playground** — response viewer with syntax-highlighted JSON, per-call timing line, token picker fed by tokens captured client-side at create time (full plaintext held in memory post-POST) plus manual paste fallback. NOT fed from `GET /api/tokens` (masked previews only).
- **Settings/auth** — behavior unchanged; restyle only.

## 4. Structure & rename plan

### Rename commit (atomic)

One commit contains everything; CI/Docker builds break otherwise:

- `git mv apps/admin web`
- `.github/workflows/ci.yml` — `working-directory`, `cache-dependency-path`
- `Dockerfile` — both COPY stages referencing `apps/admin/`
- `.dockerignore`, `.gitignore` (`/apps/admin/*`)
- `.env.example` — build + `ADMIN_SPA_DIR` comment references
- `docs/ops/*` path references
- Root `AGENTS.md` path references

(Ignore `.superpowers/sdd/*.diff` mentions — historical archives.)

### Code structure

Current `features/` layout is sound; additions only:

- `features/dashboard/` — queries (reuse stats patterns), SVG chart component, KPI/composition components split per ~350-line file cap.
- `lib/` gains spend query options.
- `_auth/index.tsx` redirects to `/dashboard`; routeTree regenerates.

## 5. Visual direction — Cobalt v2 refresh

- Keep `tokens.css` OKLCH system, fonts (Space Grotesk/Inter/JetBrains Mono), hairline-panel shell, mono labels.
- Tighter 4/8px spacing rhythm; denser tables (~40px rows); tabular numerals on all numeric columns.
- Light/dark palettes defined once in tokens; follow `prefers-color-scheme`.
- Dashboard hierarchy: KPI tiles as bordered stat blocks (no card shadows); chart series colors from tokens (ink/cobalt/graphite).
- Consolidated status semantics: cobalt = neutral accent, amber = warning, red = error, green = success — uniform across tables/chips/dashboard.

## Testing & verification

- `npm run typecheck`, `npm run check`, `npm run build` green in `web/`.
- CI admin job green after rename (paths updated atomically).
- Docker build smoke: SPA stages still produce `/admin-dist`.
- Manual smoke against running api: login → dashboard renders all four sections from live data; deep links resolve; playground create→picker flow works; dark mode via system preference.
- No new unit tests beyond what changed observable contracts require; existing `api.test.ts` keeps passing.

## Decisions log

| Decision | Choice | Rationale |
|---|---|---|
| Approach | A (existing endpoints only) | Everything needed is computable client-side; backend frozen |
| Chart | Hand-rolled SVG | Simple series; no dependency justified |
| Playground token source | Client-captured at create + paste | List endpoint masks tokens |
| Theme toggle | None; `prefers-color-scheme` | YAGNI personal scope |
| Latency percentiles | Client-side p50/p95 over ring rows | Honest label "ring window"; no backend work |
