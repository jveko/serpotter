# Web Admin Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:dispatching-parallel-agents for independent tasks to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename `apps/admin` → `web/`, add a dashboard-first IA built on existing endpoints, upgrade resource/logs/playground UX, and apply the Cobalt v2 visual refresh — with zero backend/wire changes.

**Architecture:** TanStack Router file-based routes under `web/src/routes/_auth/`, one feature module per panel (`features/<name>/` with `types.ts` / `queries.ts` / `*Panel.tsx`). All dashboard data comes from four existing endpoints (`/api/stats`, `/api/usage`, `/api/spend/*`, `/api/request-logs`) via TanStack Query options objects; pure aggregation math lives in tested utility modules; charts are hand-rolled SVG. Visual system stays on the OKLCH `tokens.css` custom-property layer.

**Tech Stack:** React 19, TypeScript strict, Vite+ (`vp`), TanStack Router + Query, vitest, no new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-08-24-web-admin-redesign-design.md` (read it first).
- **No Rust/backend changes; no new endpoints; no wire-format changes.**
- **No new npm dependencies** — charts are hand-rolled SVG; JSON highlighting is hand-rolled.
- Strict TS: zero `src/**/*.{js,jsx}`; `npm run typecheck` must stay green.
- Wire DTOs are camelCase exactly as returned by the API (see `features/*/types.ts` precedents).
- Production files soft-capped at ~350 lines excluding tests; split when approaching.
- Tests are vitest (`npm run test`); only pure logic gets unit tests (precedent: `src/lib/api.test.ts`); UI verifies via `typecheck` + `build` + manual smoke against a running api.
- Conventional commits, lowercase imperative subject, no period; NEVER `--no-verify`.
- Node ^22.18.0 || >=24.11.0.
- After any route file change, `routeTree.gen.ts` regenerates automatically on next `vp dev`/`vp build`/`tsc -b` — never hand-edit it.
- Every numeric column renders with tabular numerals (`font-variant-numeric: tabular-nums` via the `.num` utility created in Task 10).

---

### Task 1: Atomic rename `apps/admin` → `web/`

**Files:**
- Move: `apps/admin/**` → `web/**` (includes `AGENTS.md`)
- Modify: `.github/workflows/ci.yml` (admin job `working-directory`, `cache-dependency-path`)
- Modify: `Dockerfile` (both SPA-stage `COPY apps/admin/...` lines and any `cd apps/admin`)
- Modify: `.dockerignore` (`apps/admin` entries)
- Modify: `.gitignore` (`/apps/admin/*` entry)
- Modify: `.env.example` (build-path comment + `ADMIN_SPA_DIR` comment)
- Modify: `docs/ops/*.md` path references
- Modify: `AGENTS.md` (root) path references

**Interfaces:**
- Consumes: nothing.
- Produces: the SPA lives at `web/`; every live reference points there. Later tasks assume `web/src/...` paths.

- [ ] **Step 1: Move the directory**

```bash
git mv apps/admin web
```

- [ ] **Step 2: Update every live reference**

Find them all first:

```bash
grep -rn "apps/admin" --exclude-dir=.superpowers --exclude-dir=.worktrees --exclude-dir=node_modules .
```

Expected hits to fix (from pre-plan audit): `.github/workflows/ci.yml` lines ~56 (`working-directory: apps/admin`) and ~66 (`cache-dependency-path: apps/admin/package-lock.json`), `Dockerfile` two `COPY` stages, `.dockerignore`, `.gitignore`, `.env.example`, `docs/ops/*`, root `AGENTS.md`. Replace `apps/admin` → `web` in each. **Ignore** `.superpowers/sdd/*.diff` (historical archives).

- [ ] **Step 3: Verify no live references remain**

```bash
grep -rn "apps/admin" --exclude-dir=.superpowers --exclude-dir=.worktrees --exclude-dir=node_modules . || echo CLEAN
```

Expected: `CLEAN`.

- [ ] **Step 4: Verify the SPA still builds from its new home**

```bash
cd web && npm run typecheck && npm run build && npm run test
```

Expected: all three green (routeTree regeneration is path-independent; no source changes in this task).

- [ ] **Step 5: Commit atomically**

```bash
git add -A
git commit -m "build(web): rename apps/admin to web across repo"
```

The commit MUST contain the move and every reference fix together — CI/Docker break otherwise.

---

### Task 2: Dashboard data layer — types, query options, aggregation math

**Files:**
- Create: `web/src/features/dashboard/types.ts`
- Create: `web/src/features/dashboard/queries.ts`
- Create: `web/src/features/dashboard/metrics.ts`
- Test: `web/src/features/dashboard/metrics.test.ts`
- Modify: `web/src/lib/query-keys.ts`

**Interfaces:**
- Consumes: `adminFetch<T>` (`@/lib/api`), `UsageDailyDto` (`@/features/stats/types`), `RequestLogRow` (`@/features/logs/types`), `qk` pattern (`@/lib/query-keys`).
- Produces (used by Tasks 3–5):
  - `type SpendKeyRow = { keyId?: number | null; tokenName?: string | null; service: string; requests: number; cost: number }`
  - `type SpendServiceRow = { service: string; requests: number; cost: number }`
  - `spendKeysQueryOptions()`, `spendServicesQueryOptions(): UseQueryOptions` factories
  - `splitUsageWindows(rows: UsageDailyDto[], days: number): { current: UsageDailyDto[]; previous: UsageDailyDto[] }`
  - `windowTotals(rows: UsageDailyDto[]): WindowTotals` where `type WindowTotals = { requests: number; successes: number; errors: number; tokens: number; cost: number }`
  - `errorRate(totals: WindowTotals): number | null` (null when `requests === 0`)
  - `perDayByService(rows: UsageDailyDto[]): PerDaySeries` where `PerDaySeries = { dates: string[]; series: Record<string, number[]>; errorLine: number[] }`
  - `percentile(values: number[], p: number): number | null` (null on empty input)

- [ ] **Step 1: Write failing tests**

Create `web/src/features/dashboard/metrics.test.ts`:

```ts
import { describe, expect, it } from "vitest";

import type { UsageDailyDto } from "@/features/stats/types";

import {
  errorRate,
  perDayByService,
  percentile,
  splitUsageWindows,
  windowTotals,
} from "./metrics";

function row(over: Partial<UsageDailyDto>): UsageDailyDto {
  return {
    service: "tavily",
    providerUsed: "tavily",
    date: "2026-08-20",
    requests: 10,
    successes: 9,
    errors: 1,
    tokens: 100,
    cost: 0.5,
    ...over,
  };
}

describe("splitUsageWindows", () => {
  it("splits rows into current and previous windows by date cutoff", () => {
    const rows = [
      row({ date: "2026-08-01", requests: 1 }),
      row({ date: "2026-08-08", requests: 2 }),
      row({ date: "2026-08-15", requests: 3 }),
      row({ date: "2026-08-20", requests: 4 }),
    ];
    const { current, previous } = splitUsageWindows(rows, 7);
    expect(current.map((r) => r.requests)).toEqual([3, 4]);
    expect(previous.map((r) => r.requests)).toEqual([1, 2]);
  });

  it("handles an empty payload", () => {
    expect(splitUsageWindows([], 7)).toEqual({ current: [], previous: [] });
  });
});

describe("windowTotals / errorRate", () => {
  it("sums fields across rows", () => {
    const totals = windowTotals([
      row({ requests: 10, errors: 1, tokens: 100, cost: 0.5 }),
      row({ requests: 5, errors: 2, tokens: 50, cost: 0.25 }),
    ]);
    expect(totals).toEqual({
      requests: 15,
      successes: 18,
      errors: 3,
      tokens: 150,
      cost: 0.75,
    });
  });

  it("returns null error rate on zero traffic", () => {
    expect(errorRate(windowTotals([]))).toBeNull();
  });

  it("computes error rate", () => {
    expect(errorRate(windowTotals([row({ requests: 10, errors: 3 })]))).toBeCloseTo(0.3);
  });
});

describe("perDayByService", () => {
  it("pivots rows into dense per-service arrays over a shared date axis", () => {
    const out = perDayByService([
      row({ date: "2026-08-20", service: "tavily", requests: 4, errors: 0 }),
      row({ date: "2026-08-20", service: "exa", requests: 2, errors: 0 }),
      row({ date: "2026-08-21", service: "tavily", requests: 6, errors: 0 }),
    ]);
    expect(out.dates).toEqual(["2026-08-20", "2026-08-21"]);
    expect(out.series.tavily).toEqual([4, 6]);
    expect(out.series.exa).toEqual([2, 0]);
    expect(out.errorLine).toEqual([0, 0]);
  });
});

describe("percentile", () => {
  it("returns null for empty input", () => {
    expect(percentile([], 0.95)).toBeNull();
  });

  it("linearly interpolates between adjacent order statistics", () => {
    expect(percentile([10, 20, 30, 40], 0.5)).toBe(25);
    expect(percentile([10, 20, 30, 40], 0.95)).toBe(38.5);
    expect(percentile([42], 0.95)).toBe(42);
  });
});
```

- [ ] **Step 2: Run tests, verify failure**
Run: `cd web && npx vitest run src/features/dashboard/metrics.test.ts`
Expected: FAIL — cannot resolve `./metrics`.

- [ ] **Step 3: Implement `metrics.ts`**

```ts
import type { UsageDailyDto } from "@/features/stats/types";
import type { RequestLogRow } from "@/features/logs/types";

export type WindowTotals = {
  requests: number;
  successes: number;
  errors: number;
  tokens: number;
  cost: number;
};

export type PerDaySeries = {
  dates: string[];
  /** service -> requests per date (dense; 0 where absent) */
  series: Record<string, number[]>;
  /** total errors per date */
  errorLine: number[];
};

const EMPTY_TOTALS: WindowTotals = {
  requests: 0,
  successes: 0,
  errors: 0,
  tokens: 0,
  cost: 0,
};

/**
 * Split usage rows into the most-recent `days` window and the equal-sized
 * window before it, by ISO date string comparison (dates are YYYY-MM-DD).
 */
export function splitUsageWindows(
  rows: UsageDailyDto[],
  days: number,
): { current: UsageDailyDto[]; previous: UsageDailyDto[] } {
  if (rows.length === 0) return { current: [], previous: [] };
  const dates = [...new Set(rows.map((r) => r.date))].sort();
  const last = dates[dates.length - 1];
  const cutoff = new Date(`${last}T00:00:00Z`);
  cutoff.setUTCDate(cutoff.getUTCDate() - days);
  const cutoffIso = cutoff.toISOString().slice(0, 10);
  return {
    current: rows.filter((r) => r.date > cutoffIso),
    previous: rows.filter((r) => r.date <= cutoffIso && r.date > shiftIso(cutoffIso, -days)),
  };
}

function shiftIso(iso: string, daysDelta: number): string {
  const d = new Date(`${iso}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + daysDelta);
  return d.toISOString().slice(0, 10);
}

export function windowTotals(rows: UsageDailyDto[]): WindowTotals {
  return rows.reduce<WindowTotals>((acc, r) => ({
    requests: acc.requests + r.requests,
    successes: acc.successes + r.successes,
    errors: acc.errors + r.errors,
    tokens: acc.tokens + r.tokens,
    cost: acc.cost + r.cost,
  }), { ...EMPTY_TOTALS });
}

/** Fraction of requests that errored over the window; null when no traffic. */
export function errorRate(totals: WindowTotals): number | null {
  if (totals.requests === 0) return null;
  return totals.errors / totals.requests;
}

export function perDayByService(rows: UsageDailyDto[]): PerDaySeries {
  const dates = [...new Set(rows.map((r) => r.date))].sort();
  const services = [...new Set(rows.map((r) => r.service))].sort();
  const dateIdx = new Map(dates.map((d, i) => [d, i]));
  const series: Record<string, number[]> = {};
  const errorLine = dates.map(() => 0);
  for (const s of services) series[s] = dates.map(() => 0);
  for (const r of rows) {
    const i = dateIdx.get(r.date);
    if (i === undefined) continue;
    if (!series[r.service]) series[r.service] = dates.map(() => 0);
    series[r.service][i] += r.requests;
    errorLine[i] += r.errors;
  }
  return { dates, series, errorLine };
}

/** Linear-interpolated percentile of `values`; null when empty. */
export function percentile(values: number[], p: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = (sorted.length - 1) * p;
  const lo = Math.floor(idx);
  const hi = Math.ceil(idx);
  if (lo === hi) return sorted[lo];
  return sorted[lo] + (sorted[hi] - sorted[lo]) * (idx - lo);
}

/** Latency summary over currently loaded ring rows ("ring window"). */
export function latencySummary(rows: RequestLogRow[]): { p50: number | null; p95: number | null } {
  const durations = rows
    .map((r) => r.durationMs)
    .filter((d): d is number => typeof d === "number");
  return { p50: percentile(durations, 0.5), p95: percentile(durations, 0.95) };
}
```


- [ ] **Step 4: Run tests, verify pass**

Run: `cd web && npx vitest run src/features/dashboard/metrics.test.ts`
Expected: PASS (all cases).

- [ ] **Step 5: Add types + query options**

Create `web/src/features/dashboard/types.ts`:

```ts
/** Row from GET /api/spend/keys ('unknown' service when key deleted). */
export type SpendKeyRow = {
  keyId?: number | null;
  tokenName?: string | null;
  service: string;
  requests: number;
  cost: number;
};

/** Row from GET /api/spend/services. */
export type SpendServiceRow = {
  service: string;
  requests: number;
  cost: number;
};
```

Add to `web/src/lib/query-keys.ts` inside `qk`:

```ts
  spend: {
    all: ["spend"] as const,
    keys: () => ["spend", "keys"] as const,
    services: () => ["spend", "services"] as const,
  },
  dashboard: {
    all: ["dashboard"] as const,
  },
```

Create `web/src/features/dashboard/queries.ts`:

```ts
import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { SpendKeyRow, SpendServiceRow } from "./types";

export function spendKeysQueryOptions() {
  return queryOptions({
    queryKey: qk.spend.keys(),
    queryFn: () => adminFetch<SpendKeyRow[]>("/api/spend/keys"),
    staleTime: 60_000,
  });
}

export function spendServicesQueryOptions() {
  return queryOptions({
    queryKey: qk.spend.services(),
    queryFn: () => adminFetch<SpendServiceRow[]>("/api/spend/services"),
    staleTime: 60_000,
  });
}
```

(The dashboard reuses `usageQueryOptions(days)` from `@/features/stats/queries` with `days = 2 × selectedWindow` so both windows arrive in one fetch, and `requestLogs.list({ limit: 8 })` for activity.)

- [ ] **Step 6: Typecheck + full test suite**

Run: `cd web && npm run typecheck && npm run test`
Expected: green.

- [ ] **Step 7: Commit**

```bash
git add web/src/features/dashboard web/src/lib/query-keys.ts
git commit -m "feat(web): dashboard data layer with window aggregation"
```

---

### Task 3: KPI strip component

**Files:**
- Create: `web/src/features/dashboard/KpiStrip.tsx`

**Interfaces:**
- Consumes: `WindowTotals`, `errorRate` from `./metrics`; `StatsDto` from `@/features/stats/types`.
- Produces: `KpiStrip({ totals, previousTotals, stats }: { totals: WindowTotals; previousTotals: WindowTotals | null; stats: StatsDto })` — presentational only, no fetching.

- [ ] **Step 1: Implement the component**

Four bordered stat tiles (no shadows — spec §5). Requests tile shows delta vs previous window (`▲/▼ x%` vs previous, omitted when `previousTotals` is null or previous requests is 0). Error-rate chip thresholds: neutral < 0.10, amber ≥ 0.10, red ≥ 0.25. Spend shows per-service split beneath (top 3 services by cost). Pool health shows `activeApiKeys/apiKeys` plus node count.

```tsx
import type { StatsDto } from "@/features/stats/types";

import { errorRate, windowTotals, type WindowTotals } from "./metrics";

function fmt(n: number): string {
  return n.toLocaleString("en-US");
}

function delta(current: number, previous: number | undefined): string | null {
  if (previous === undefined || previous === 0) return null;
  const pct = ((current - previous) / previous) * 100;
  const sign = pct >= 0 ? "▲" : "▼";
  return `${sign} ${Math.abs(pct).toFixed(0)}%`;
}

export function KpiStrip({
  totals,
  previousTotals,
  stats,
}: {
  totals: WindowTotals;
  previousTotals: WindowTotals | null;
  stats: StatsDto;
}) {
  const rate = errorRate(totals);
  const rateClass =
    rate == null ? "" : rate >= 0.25 ? "kpi__chip is-bad" : rate >= 0.1 ? "kpi__chip is-warn" : "kpi__chip";
  const reqDelta = delta(totals.requests, previousTotals?.requests);
  const topSpenders = Object.entries(
    // filled by caller-provided per-service costs via totals only; kept simple:
    {} as Record<string, number>,
  );
  void topSpenders;

  return (
    <div className="kpi-strip">
      <div className="kpi">
        <span className="kpi__label">Requests</span>
        <span className="kpi__value num">{fmt(totals.requests)}</span>
        {reqDelta ? <span className="kpi__delta num">{reqDelta}</span> : null}
      </div>
      <div className="kpi">
        <span className="kpi__label">Error rate</span>
        <span className={`kpi__value num ${rateClass}`}>
          {rate == null ? "—" : `${(rate * 100).toFixed(1)}%`}
        </span>
      </div>
      <div className="kpi">
        <span className="kpi__label">Spend</span>
        <span className="kpi__value num">${totals.cost.toFixed(2)}</span>
      </div>
      <div className="kpi">
        <span className="kpi__label">Pool</span>
        <span className="kpi__value num">
          {stats.activeApiKeys}/{stats.apiKeys} keys · {stats.nodes} nodes
        </span>
      </div>
    </div>
  );
}

// Re-exported for the page's convenience; keeps imports local to the feature.
export { windowTotals };
```

Remove the dead `topSpenders` placeholder above before committing — it was included only to note that per-service spend belongs to the SpendLeaderboard component (Task 5), not the KPI strip. Final file has no such block.

- [ ] **Step 2: Typecheck**

Run: `cd web && npm run typecheck`
Expected: green (component not yet imported anywhere — acceptable mid-task; Task 6 wires it).

- [ ] **Step 3: Commit**

```bash
git add web/src/features/dashboard/KpiStrip.tsx
git commit -m "feat(web): dashboard kpi strip"
```

---

### Task 4: Usage chart (hand-rolled SVG)

**Files:**
- Create: `web/src/features/dashboard/UsageChart.tsx`

**Interfaces:**
- Consumes: `PerDaySeries` from `./metrics`.
- Produces: `UsageChart({ data, windowDays }: { data: PerDaySeries; windowDays: number })` — pure render; colors come from CSS custom properties (`var(--ink)`, `var(--accent)`, `var(--graphite)`).

- [ ] **Step 1: Implement**

Stacked bars per day (one `<rect>` segment per service, legend chips above), thin red error overlay polyline. Fixed viewBox `720x180`, bars sized from `dates.length`. Services get deterministic fills cycling `--accent`, `--ink`, `--graphite`, `--muted` (tokens exist after Task 10; until then fall back to literal oklch values copied from `web/tokens.css`).

```tsx
import type { PerDaySeries } from "./metrics";

const W = 720;
const H = 180;
const PAD = { top: 8, right: 8, bottom: 18, left: 8 };

const FILLS = [
  "var(--accent, oklch(0.55 0.21 260))",
  "var(--ink, oklch(0.24 0.02 260))",
  "var(--graphite, oklch(0.55 0.01 260))",
  "var(--muted, oklch(0.72 0.01 260))",
];

export function UsageChart({ data, windowDays }: { data: PerDaySeries; windowDays: number }) {
  const { dates, series, errorLine } = data;
  const services = Object.keys(series);
  if (dates.length === 0) {
    return <p className="empty">No usage recorded in this window.</p>;
  }
  const maxTotal = Math.max(
    1,
    ...dates.map((_, i) => services.reduce((sum, s) => sum + series[s][i], 0)),
  );
  const innerW = W - PAD.left - PAD.right;
  const innerH = H - PAD.top - PAD.bottom;
  const slotW = innerW / dates.length;
  const barW = Math.max(2, slotW * 0.7);

  const errPoints = dates
    .map((_, i) => {
      const x = PAD.left + slotW * i + slotW / 2;
      const y = PAD.top + innerH - (errorLine[i] / maxTotal) * innerH;
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");

  return (
    <figure className="usage-chart">
      <figcaption className="usage-chart__legend">
        {services.map((s, i) => (
          <span key={s} className="usage-chart__key">
            <i style={{ background: FILLS[i % FILLS.length] }} />
            {s}
          </span>
        ))}
        <span className="usage-chart__key usage-chart__key--line">
          <i className="usage-chart__errswatch" />
          errors
        </span>
        <span className="usage-chart__window">{windowDays}d window</span>
      </figcaption>
      <svg viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Requests per day by service">
        {dates.map((date, i) => {
          let yCursor = PAD.top + innerH;
          const x = PAD.left + slotW * i + (slotW - barW) / 2;
          return (
            <g key={date}>
              {services.map((s, si) => {
                const h = (series[s][i] / maxTotal) * innerH;
                if (h <= 0) return null;
                yCursor -= h;
                return (
                  <rect
                    key={s}
                    x={x}
                    y={yCursor}
                    width={barW}
                    height={h}
                    fill={FILLS[si % FILLS.length]}
                  >
                    <title>{`${date} ${s}: ${series[s][i]}`}</title>
                  </rect>
                );
              })}
            </g>
          );
        })}
        <polyline points={errPoints} fill="none" stroke="var(--bad, oklch(0.55 0.22 25))" strokeWidth="1.5" />
      </svg>
    </figure>
  );
}
```

- [ ] **Step 2: Typecheck**

Run: `cd web && npm run typecheck` — expected green.

- [ ] **Step 3: Commit**

```bash
git add web/src/features/dashboard/UsageChart.tsx
git commit -m "feat(web): svg usage chart stacked by service"
```

---

### Task 5: Spend leaderboard, pool health row, recent activity feed

**Files:**
- Create: `web/src/features/dashboard/SpendLeaderboard.tsx`
- Create: `web/src/features/dashboard/PoolHealth.tsx`
- Create: `web/src/features/dashboard/RecentActivity.tsx`

**Interfaces:**
- Consumes: `SpendKeyRow`/`SpendServiceRow` (`./types`), `StatsDto` (`@/features/stats/types`), `KeyRow` (`@/features/keys/types`), `NodeRow` (`@/features/nodes/types`), `RequestLogRow` (`@/features/logs/types`).
- Produces:
  - `SpendLeaderboard({ keys, services }: { keys: SpendKeyRow[]; services: SpendServiceRow[] })`
  - `PoolHealth({ stats, keys, nodes }: { stats: StatsDto; keys: KeyRow[]; nodes: NodeRow[] })`
  - `RecentActivity({ rows }: { rows: RequestLogRow[] })`

- [ ] **Step 1: Implement SpendLeaderboard**

Two columns: top 5 keys by cost (label = `tokenName ?? keyId`, share bar scaled to max cost) and all services with request-share bars. Deep-link key labels to `/keys?focus=<id>` when `keyId != null`.

```tsx
import { Link } from "@tanstack/react-router";

import type { SpendKeyRow, SpendServiceRow } from "./types";

function ShareBar({ value, max }: { value: number; max: number }) {
  const pct = max > 0 ? (value / max) * 100 : 0;
  return (
    <span className="share-bar" aria-hidden>
      <i style={{ width: `${pct}%` }} />
    </span>
  );
}

export function SpendLeaderboard({ keys, services }: { keys: SpendKeyRow[]; services: SpendServiceRow[] }) {
  const topKeys = [...keys].sort((a, b) => b.cost - a.cost).slice(0, 5);
  const maxKeyCost = Math.max(0, ...topKeys.map((k) => k.cost));
  const maxSvcReq = Math.max(1, ...services.map((s) => s.requests));

  return (
    <div className="leaderboards">
      <section className="panel-section">
        <h3>Top spending keys</h3>
        {topKeys.length === 0 ? (
          <p className="empty">No spend recorded.</p>
        ) : (
          <ul className="lb-list">
            {topKeys.map((k) => (
              <li key={`${k.keyId ?? "?"}-${k.tokenName ?? "?"}`}>
                <span className="lb-label">
                  {k.keyId != null ? (
                    <Link to="/keys" search={{ focus: k.keyId }}>{k.tokenName ?? `key #${k.keyId}`}</Link>
                  ) : (
                    k.tokenName ?? "unknown"
                  )}
                </span>
                <ShareBar value={k.cost} max={maxKeyCost} />
                <span className="num lb-value">${k.cost.toFixed(2)}</span>
              </li>
            ))}
          </ul>
        )}
      </section>
      <section className="panel-section">
        <h3>Spend by service</h3>
        <ul className="lb-list">
          {services.map((s) => (
            <li key={s.service}>
              <span className="lb-label">{s.service}</span>
              <ShareBar value={s.requests} max={maxSvcReq} />
              <span className="num lb-value">${s.cost.toFixed(2)}</span>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
```

- [ ] **Step 2: Implement PoolHealth**

Credit bar per `stats.byService` row (`creditsRemaining`/`creditsLimit`; both-null → hatched neutral class `credit-bar is-unknown`). Chips: count of keys with `consecutiveFails > 0` (amber) linking `/keys`, disabled/failing nodes (`!enabled || lastError != null`) linking `/nodes`, red when > 0.

```tsx
import { Link } from "@tanstack/react-router";

import type { KeyRow } from "@/features/keys/types";
import type { NodeRow } from "@/features/nodes/types";
import type { StatsDto } from "@/features/stats/types";

function CreditBar({ remaining, limit }: { remaining: number | null | undefined; limit: number | null | undefined }) {
  if (remaining == null || limit == null || limit <= 0) {
    return <span className="credit-bar is-unknown" title="credits unknown" aria-hidden />;
  }
  const pct = Math.max(0, Math.min(100, (remaining / limit) * 100));
  return (
    <span
      className={`credit-bar ${pct < 20 ? "is-low" : ""}`}
      title={`${remaining}/${limit}`}
      aria-hidden
    >
      <i style={{ width: `${pct}%` }} />
    </span>
  );
}

export function PoolHealth({ stats, keys, nodes }: { stats: StatsDto; keys: KeyRow[]; nodes: NodeRow[] }) {
  const failingKeys = keys.filter((k) => k.consecutiveFails > 0).length;
  const badNodes = nodes.filter((n) => !n.enabled || n.lastError != null).length;
  return (
    <div className="pool-health">
      {stats.byService.map((s) => (
        <div key={s.service} className="pool-health__svc">
          <span className="lb-label">{s.service}</span>
          <CreditBar remaining={s.creditsRemaining} limit={s.creditsLimit} />
          <span className="num pool-health__count">{s.active}/{s.keys}</span>
        </div>
      ))}
      <div className="pool-health__alerts">
        <Link to="/keys" className={`chip ${failingKeys > 0 ? "chip--warn" : ""}`}>
          {failingKeys} failing {failingKeys === 1 ? "key" : "keys"}
        </Link>
        <Link to="/nodes" className={`chip ${badNodes > 0 ? "chip--bad" : ""}`}>
          {badNodes} node issues
        </Link>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Implement RecentActivity**

Last N ring rows as a compact list: status dot (`is-ok` <400, `is-warn` <500, `is-bad` ≥500), path, service, duration, relative time. Each row links to `/logs?requestId=<id>` when present else `/logs`.

```tsx
import { Link } from "@tanstack/react-router";

import type { RequestLogRow } from "@/features/logs/types";

function relativeTime(iso: string): string {
  const ms = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(ms / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}

export function RecentActivity({ rows }: { rows: RequestLogRow[] }) {
  if (rows.length === 0) return <p className="empty">No recent requests.</p>;
  return (
    <ul className="activity-list">
      {rows.map((r) => (
        <li key={r.id}>
          <Link
            to="/logs"
            search={r.requestId ? { requestId: r.requestId } : {}}
            className={`activity-row status-${r.status >= 500 ? "bad" : r.status >= 400 ? "warn" : "ok"}`}
          >
            <span className="dot" aria-hidden />
            <span className="activity-path">{r.path}</span>
            <span className="activity-svc">{r.service ?? "—"}</span>
            <span className="num">{r.durationMs != null ? `${r.durationMs}ms` : "—"}</span>
            <time dateTime={r.createdAt}>{relativeTime(r.createdAt)}</time>
          </Link>
        </li>
      ))}
    </ul>
  );
}
```

Note: `/logs` currently validates no search params. Adding `search={{ requestId }}` here requires the route to accept it — Task 8 adds `requestId` (and `status` presets) to the logs route validation. Until Task 8 lands, keep these props but expect typecheck failure if building out of order; execute Tasks 5→8 in order within the same branch before running gates.

- [ ] **Step 4: Commit**

```bash
git add web/src/features/dashboard/SpendLeaderboard.tsx web/src/features/dashboard/PoolHealth.tsx web/src/features/dashboard/RecentActivity.tsx
git commit -m "feat(web): spend leaderboard, pool health, activity feed"
```

---

### Task 6: Dashboard route, default landing, navigation wiring

**Files:**
- Create: `web/src/routes/_auth/dashboard.tsx`
- Create: `web/src/routes/_auth/index.tsx`
- Modify: `web/src/lib/constants.ts` (SECTIONS)
- Modify: `web/src/features/shell/Sidebar.tsx` (`SECTION_TO`, wordmark link)
- Modify: `web/src/features/shell/Topbar.tsx` (refresh map gains dashboard)
- Modify: `web/src/features/shell/Cmdk.tsx` (dashboard actions)

**Interfaces:**
- Consumes: everything from Tasks 2–5.
- Produces: `/dashboard` route with validated search `{ days?: number }` (default 14); `_auth/index` redirects to `/dashboard`.

- [ ] **Step 1: Route file**

```tsx
import { createFileRoute, Link } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";

import { KpiStrip } from "@/features/dashboard/KpiStrip";
import { PoolHealth } from "@/features/dashboard/PoolHealth";
import { RecentActivity } from "@/features/dashboard/RecentActivity";
import { SpendLeaderboard } from "@/features/dashboard/SpendLeaderboard";
import { UsageChart } from "@/features/dashboard/UsageChart";
import { splitUsageWindows, windowTotals } from "@/features/dashboard/metrics";
import { spendKeysQueryOptions, spendServicesQueryOptions } from "@/features/dashboard/queries";
import { keysQueryOptions } from "@/features/keys/queries";
import { requestLogsQueryOptions } from "@/features/logs/queries"; // adapt to actual export name in features/logs/queries.ts
import { nodesQueryOptions } from "@/features/nodes/queries";
import { statsQueryOptions, usageQueryOptions } from "@/features/stats/queries";

type DashboardSearch = { days?: number };

const DAYS_CHOICES = [7, 14, 30, 90];

export const Route = createFileRoute("/_auth/dashboard")({
  validateSearch: (search: Record<string, unknown>): DashboardSearch => {
    const raw = Number(search.days);
    const days = DAYS_CHOICES.includes(raw) ? raw : undefined;
    return days ? { days } : {};
  },
  component: DashboardPage,
});

const WINDOW_DAYS_DEFAULT = 14;

function DashboardPage() {
  const { days = WINDOW_DAYS_DEFAULT } = Route.useSearch();

  const statsQ = useQuery(statsQueryOptions);
  const usageQ = useQuery(usageQueryOptions(days * 2)); // current + previous windows
  const spendKeysQ = useQuery(spendKeysQueryOptions());
  const spendSvcQ = useQuery(spendServicesQueryOptions());
  const keysQ = useQuery(keysQueryOptions);
  const nodesQ = useQuery(nodesQueryOptions);
  const activityQ = useQuery(requestLogsQueryOptions({ limit: 8 }));

  const { current, previous } = splitUsageWindows(usageQ.data ?? [], days);
  const totals = windowTotals(current);
  const prevTotals = previous.length > 0 ? windowTotals(previous) : null;

  return (
    <section className="page page--dashboard" aria-label="Dashboard">
      <header className="page__head">
        <h2>Dashboard</h2>
        <nav className="window-picker" aria-label="Usage window">
          {DAYS_CHOICES.map((d) => (
            <Link
              key={d}
              to="/dashboard"
              search={{ days: d }}
              className={`window-picker__opt ${d === days ? "is-active" : ""}`}
            >
              {d}d
            </Link>
          ))}
        </nav>
      </header>

      {statsQ.data ? (
        <KpiStrip totals={totals} previousTotals={prevTotals} stats={statsQ.data} />
      ) : null}

      <UsageChart data={chartData(usageQ.data ?? [], days)} windowDays={days} />

      {spendKeysQ.data && spendSvcQ.data ? (
        <SpendLeaderboard keys={spendKeysQ.data} services={spendSvcQ.data} />
      ) : null}

      {statsQ.data && keysQ.data && nodesQ.data ? (
        <PoolHealth stats={statsQ.data} keys={keysQ.data} nodes={nodesQ.data} />
      ) : null}

      <RecentActivity rows={activityQ.data ?? []} />
    </section>
  );
}
```

`chartData` is `perDayByService(current)` imported from `./metrics` — replace the call accordingly (import it alongside `splitUsageWindows`). Adapt the three `*QueryOptions` import names to the real exports in `features/{logs,keys,nodes}/queries.ts` (they exist per the scout map; e.g. logs exports a builder consumed with filters `{ limit: 8 }`).

- [ ] **Step 2: Default landing redirect**

`web/src/routes/_auth/index.tsx`:

```tsx
import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/_auth/")({
  beforeLoad: () => {
    throw redirect({ to: "/dashboard" });
  },
});
```

- [ ] **Step 3: Navigation wiring**

`constants.ts`: add `"dashboard"` to `SectionId` union and prepend `{ id: "dashboard", label: "Dashboard" }` to `SECTIONS` (keep `stats` — the old panel remains as the inventory/usage-table page).

`Sidebar.tsx`: add `dashboard: "/dashboard"` to `SECTION_TO`; change the wordmark `Link to="/stats"` → `to="/dashboard"`.

`Topbar.tsx`: extend the panel→query-key-prefix refresh map with `dashboard: qk.dashboard.all` (plus it already invalidates stats/usage via prefixes — verify the existing switch handles the new id; add a case mirroring `stats` since the dashboard shares those queries).

`Cmdk.tsx`: add a "Go to dashboard" action navigating to `/dashboard`.

- [ ] **Step 4: Regenerate routes + gates**

Run: `cd web && npm run typecheck && npm run test && npm run build`
Expected: green. `routeTree.gen.ts` picks up `_auth/dashboard` and `_auth/index`.

If typecheck fails on `/keys` or `/logs` search params (`focus`, `requestId`), those routes gain their validations in Tasks 7–8 — land Tasks 6–8 before the final gate of this sequence.

- [ ] **Step 5: Commit**

```bash
git add web/src/routes web/src/lib/constants.ts web/src/features/shell web/src/routeTree.gen.ts
git commit -m "feat(web): dashboard route as default landing"
```

---

### Task 7: Keys panel — inline actions + focus deep-link

**Files:**
- Modify: `web/src/routes/_auth/keys.tsx` (validate `focus` search param)
- Modify: `web/src/features/keys/KeysPanel.tsx`

**Interfaces:**
- Consumes: existing key mutations in `features/keys/queries.ts` (toggle/rotate/delete hooks already exist).
- Produces: `/keys?focus=<id>` scrolls to and highlights the row (`data-focus` attribute + CSS class); toggle/rotate/delete operate inline per-row; rotate uses the existing PUT mutation inside an inline dialog; delete requires confirm.

- [ ] **Step 1: Route search validation**

In `routes/_auth/keys.tsx` add:

```ts
validateSearch: (search: Record<string, unknown>): { focus?: number } => {
  const raw = Number(search.focus);
  return Number.isInteger(raw) && raw > 0 ? { focus: raw } : {};
},
```

- [ ] **Step 2: Panel behavior**

In `KeysPanel.tsx`:
1. Read `focus` via `Route.useSearch()`; on rows render, set `data-focus={k.id === focus || undefined}` on the `<tr>` and add `className="row-focus"` when matched; add a `useEffect` scrolling `document.querySelector('[data-focus]')?.scrollIntoView({ block: "center" })` once when `focus` changes.
2. Move toggle/delete into per-row buttons wired to the existing mutations (they already invalidate `qk.keys.all` + stats via `invalidateKeysAndStats`).
3. Rotate opens a small dialog (reuse the Base UI dialog primitive already used by create) prefilled with the current `service`, password-style field for the new key, submits the existing `PUT /api/keys/{id}` mutation.
4. Keep sync-credits toolbar button and report toast unchanged.

CSS (added in Task 10): `tr[data-focus] { outline: 2px solid var(--accent); outline-offset: -2px; }`.

- [ ] **Step 3: Gates + commit**

Run: `cd web && npm run typecheck && npm run build`
Expected: green (`/keys` now accepts `focus`, satisfying Task 5–6 links).

```bash
git add web/src/routes/_auth/keys.tsx web/src/features/keys/KeysPanel.tsx
git commit -m "feat(web): inline key actions with focus deep-link"
```

---

### Task 8: Logs — status presets, expandable detail, latency summary

**Files:**
- Modify: `web/src/routes/_auth/logs.tsx` (accept `requestId` + `status` search params)
- Modify: `web/src/features/logs/LogsPanel.tsx`
- Create: `web/src/features/logs/RowDetail.tsx`

**Interfaces:**
- Consumes: `latencySummary` from `@/features/dashboard/metrics`; existing filter state/debouncer in `LogsPanel.tsx`.
- Produces: preset chips `All / 2xx / 4xx / 5xx` driving the existing `status` filter (`""` | `"2"` | `"4"` | `"5"` prefix match — server does prefix matching on status); expandable row detail holding `strategy`, `providersConsulted`, `attemptCount`, `keyId`, `nodeId`, `requestId`, `queryPreview`, `errorKind`; header line `p50/p95 (ring window)` computed from loaded rows.

- [ ] **Step 1: Route params**

Extend `validateSearch` in `routes/_auth/logs.tsx`:

```ts
validateSearch: (search: Record<string, unknown>): { requestId?: string; status?: string } => ({
  requestId: typeof search.requestId === "string" && search.requestId ? search.requestId : undefined,
  status: typeof search.status === "string" && /^[245]$/.test(search.status) ? search.status : undefined,
}),
```

Seed the filter draft from these params on mount (`useEffect` on `search` → set filter state) so Task 5's deep links land filtered.

- [ ] **Step 2: Presets + latency header + collapsible columns**

In `LogsPanel.tsx`:
1. Chip row above the table: `["all","2","4","5"]` — active chip sets `filters.status` to `""`/`"2"`/`"4"`/`"5"` through the existing debounced setter.
2. Header meta line: `const { p50, p95 } = latencySummary(data ?? [])` rendered as `p50 {p50 ?? "—"}ms · p95 {p95 ?? "—"}ms (ring window)`.
3. Remove the five low-signal `<th>`/`<td>` column pairs (`strategy`, `providersConsulted`, `attemptCount`, `keyId`, `nodeId`) from the main table; add a trailing chevron cell toggling `expandedId` state; expanded rows render `<RowDetail row={r} />` in a full-width `<td colSpan>` beneath.

Create `web/src/features/logs/RowDetail.tsx`:

```tsx
import type { RequestLogRow } from "./types";

export function RowDetail({ row }: { row: RequestLogRow }) {
  const pairs: [string, string][] = [
    ["strategy", row.strategy ?? "—"],
    ["providers consulted", row.providersConsulted ?? "—"],
    ["attempts", row.attemptCount?.toString() ?? "—"],
    ["key id", row.keyId?.toString() ?? "—"],
    ["node id", row.nodeId?.toString() ?? "—"],
    ["request id", row.requestId ?? "—"],
    ["query", row.queryPreview ?? "—"],
    ["error kind", row.errorKind ?? "—"],
    ["provider", row.providerUsed ?? "—"],
    ["token", row.tokenName ?? "—"],
  ];
  return (
    <dl className="row-detail">
      {pairs.map(([k, v]) => (
        <div key={k} className="row-detail__pair">
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}
```

- [ ] **Step 3: Gates + commit**

Run: `cd web && npm run typecheck && npm run test && npm run build` — expected green (existing `api.test.ts` unaffected).

```bash
git add web/src/routes/_auth/logs.tsx web/src/features/logs
git commit -m "feat(web): log presets, expandable detail, latency summary"
```

---

### Task 9: Tokens dialog + captured-token registry for playground

**Files:**
- Create: `web/src/features/tokens/captured-tokens.ts`
- Modify: `web/src/features/tokens/TokensPanel.tsx` (create flow → dialog with copy)
- Modify: `web/src/features/playground/runPlayground.ts` + playground panel (picker)

**Interfaces:**
- Consumes: existing `POST /api/tokens` mutation returning one-shot `{ id, name, token, createdAt }`; localStorage keys from `@/lib/constants`.
- Produces: `rememberCapturedToken(id: number, name: string, plaintext: string): void` and `listCapturedTokens(): CapturedToken[]` where `CapturedToken = { id: number; name: string; plaintext: string }` — session-scoped (in-module Map; deliberately NOT persisted to storage: plaintext tokens must not outlive the tab).

- [ ] **Step 1: Registry module**

```ts
export type CapturedToken = { id: number; name: string; plaintext: string };

/** One-shot plaintexts captured at create time; session-scoped by design. */
const captured = new Map<number, CapturedToken>();

export function rememberCapturedToken(id: number, name: string, plaintext: string): void {
  captured.set(id, { id, name, plaintext });
}

export function listCapturedTokens(): CapturedToken[] {
  return [...captured.values()];
}
```

- [ ] **Step 2: Tokens create dialog**

Replace the inline banner create flow in `TokensPanel.tsx` with a Base UI dialog (same primitive as keys rotate): fields name → submit calls the existing create mutation → on success, `rememberCapturedToken(res.id, res.name, res.token)` and render the reveal step INSIDE the dialog with a copy button (`navigator.clipboard.writeText(res.token)` + "Copied" feedback). Dialog closes only via user dismiss. List unchanged apart from relative `createdAt` (reuse the `relativeTime` helper — extract it to `web/src/lib/relative-time.ts` and import from both RecentActivity and here).

- [ ] **Step 3: Playground picker**

Above the token input in the playground panel: if `listCapturedTokens()` is non-empty, render a `<select>` listing captured tokens (label `name`); selecting one fills the token field. Manual paste remains the primary path. No persistence.

- [ ] **Step 4: Gates + commit**

Run: `cd web && npm run typecheck && npm run build` — expected green.

```bash
git add web/src/features/tokens web/src/features/playground web/src/lib/relative-time.ts
git commit -m "feat(web): token dialog with captured-token playground picker"
```

---

### Task 10: Nodes test chip + Cobalt v2 visual pass

**Files:**
- Modify: `web/src/features/nodes/NodesPanel.tsx` (test result as row chip)
- Modify: `web/tokens.css` (density scale, status semantics, dark palette)
- Modify: `web/src/styles.css` (table density, `.num`, focus ring, dashboard layout classes)

**Interfaces:**
- Consumes: existing node test mutation returning `{ ok, latencyMs?, error? }`.
- Produces: consolidated status classes `is-ok/is-warn/is-bad/chip--warn/chip--bad`, `.num` tabular utility, denser tables (~40px rows), `prefers-color-scheme` dark palette.

- [ ] **Step 1: Node test result inline**

In `NodesPanel.tsx`, store last test result per node id (`useState<Record<number, {ok: boolean; latencyMs?: number | null; error?: string | null}>>`) and render a chip in the row's actions cell: `ok` → green `12ms`, failure → red truncated `lastError`. Keep toast removal optional — replace it.

- [ ] **Step 2: tokens.css additions**

Append (do not restructure existing tokens):

```css
/* Status semantics (consolidated) */
--ok: oklch(0.72 0.17 145);
--warn: oklch(0.8 0.16 85);
--bad: oklch(0.55 0.22 25);

/* Density rhythm */
--space-1: 4px;
--space-2: 8px;
--row-h: 40px;

@media (prefers-color-scheme: dark) {
  /* mirror the light palette onto dark paper values already used by the shell */
  --paper: oklch(0.18 0.01 260);
  --ink: oklch(0.92 0.01 260);
  --muted: oklch(0.62 0.01 260);
  --hairline: oklch(0.3 0.01 260);
}
```

(Adjust property names to whatever the existing `tokens.css` actually defines — read the file first and mirror ITS names; do not invent parallel systems.)

- [ ] **Step 3: styles.css additions**

Utility + component classes referenced by earlier tasks:

```css
.num { font-variant-numeric: tabular-nums; }

.kpi-strip { display: grid; grid-template-columns: repeat(4, 1fr); gap: var(--space-2); }
.kpi { border: 1px solid var(--hairline); padding: var(--space-2) var(--space-3, 12px); display: flex; flex-direction: column; gap: 2px; }
.kpi__label { font-family: var(--font-mono); font-size: 11px; text-transform: uppercase; letter-spacing: 0.08em; color: var(--muted); }
.kpi__value { font-size: 20px; font-weight: 600; }
.kpi__chip.is-warn { color: var(--warn); }
.kpi__chip.is-bad { color: var(--bad); }
.kpi__delta { font-size: 12px; color: var(--muted); }

.window-picker { display: flex; gap: var(--space-1); }
.window-picker__opt { padding: 2px 8px; border: 1px solid var(--hairline); font-size: 12px; }
.window-picker__opt.is-active { border-color: var(--accent); color: var(--accent); }

.usage-chart { border: 1px solid var(--hairline); padding: var(--space-2); }
.usage-chart__legend { display: flex; gap: var(--space-3, 12px); align-items: center; font-size: 12px; margin-bottom: var(--space-2); }
.usage-chart__key i { display: inline-block; width: 10px; height: 10px; margin-right: 4px; }
.usage-chart__errswatch { display: inline-block; width: 12px; height: 0; border-top: 2px solid var(--bad); vertical-align: middle; }
.usage-chart__window { margin-left: auto; color: var(--muted); font-family: var(--font-mono); }

.leaderboards { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-2); }
.lb-list { list-style: none; margin: 0; padding: 0; display: grid; gap: var(--space-1); }
.lb-list li { display: grid; grid-template-columns: minmax(80px, auto) 1fr auto; align-items: center; gap: var(--space-2); min-height: 28px; }
.lb-value { text-align: right; }
.share-bar { display: block; height: 6px; background: transparent; position: relative; }
.share-bar i { position: absolute; inset: 0 auto 0 0; background: var(--accent); opacity: 0.7; }

.credit-bar { display: inline-block; width: 120px; height: 8px; border: 1px solid var(--hairline); position: relative; }
.credit-bar i { position: absolute; inset: 0 auto 0 0; background: var(--accent); }
.credit-bar.is-low i { background: var(--warn); }
.credit-bar.is-unknown { background-image: repeating-linear-gradient(45deg, var(--hairline) 0 2px, transparent 2px 5px); }

.pool-health { display: grid; gap: var(--space-2); }
.pool-health__svc { display: grid; grid-template-columns: 80px auto auto; gap: var(--space-2); align-items: center; }
.pool-health__alerts { display: flex; gap: var(--space-2); }
.chip--warn { border-color: var(--warn); color: var(--warn); }
.chip--bad { border-color: var(--bad); color: var(--bad); }

.activity-list { list-style: none; padding: 0; margin: 0; display: grid; gap: 2px; }
.activity-row { display: grid; grid-template-columns: 10px 1fr auto auto auto; gap: var(--space-2); align-items: center; min-height: var(--row-h); padding: 0 var(--space-2); }
.activity-row .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); }
.activity-row.status-ok .dot { background: var(--ok); }
.activity-row.status-warn .dot { background: tr(var(--warn)); background: var(--warn); }
.activity-row.status-bad .dot { background: var(--bad); }

tr[data-focus] { outline: 2px solid var(--accent); outline-offset: -2px; }

.row-detail { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: var(--space-2); padding: var(--space-2); margin: 0; }
.row-detail dt { font-family: var(--font-mono); font-size: 11px; color: var(--muted); text-transform: uppercase; }
.row-detail dd { margin: 0; overflow-wrap: anywhere; }
```

Also tighten the generic data table rules: reduce cell padding to `var(--space-1) var(--space-2)` and set `min-height: var(--row-h)` on `tbody tr`. Apply `class="num"` in JSX where numeric cells render (KPI values, spend, durations, counts) — done implicitly by earlier tasks' markup; sweep remaining tables.

Fix the accidental duplicate `background` line in `.activity-row.status-warn` before committing (keep `background: var(--warn);`).

- [ ] **Step 4: Full gates**

Run: `cd web && npm run typecheck && npm run check && npm run test && npm run build`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add web/src web/tokens.css
git commit -m "style(web): cobalt v2 density, status semantics, dark mode"
```

---

### Task 11: End-to-end verification + docs

**Files:**
- Modify: `web/AGENTS.md` (paths, dashboard section, new modules)
- Modify: `AGENTS.md` (root) — swap remaining `apps/admin` mentions if Task 1 missed doc-level prose
- Modify: `docs/superpowers/specs/2026-08-24-web-admin-redesign-design.md` — none (spec frozen)

**Interfaces:**
- Consumes: completed Tasks 1–10.
- Produces: verified deliverable + updated project knowledge base.

- [ ] **Step 1: Live smoke against a running api**

```bash
set -a; source .env; set +a
export ADMIN_SECRET=dev-admin
cargo run -p serpotter-api -- &   # or use the usual dev flow
cd web && npm run dev             # http://localhost:5173/
```

Verify manually: login → lands on `/dashboard`; KPI tiles populated; window picker updates URL `?days=` and refetches; chart renders stacked bars; spend leaderboards non-empty (seed data if needed via seed-key/seed-token); failing-key chip deep-links to highlighted `/keys?focus=` row; activity rows deep-link to filtered `/logs`; tokens dialog captures plaintext → playground picker offers it; system dark mode flips the palette.

- [ ] **Step 2: CI sanity (no execution needed)**

Re-grep for stale references: `grep -rn "apps/admin" --exclude-dir=.superpowers --exclude-dir=.worktrees --exclude-dir=node_modules .` → `CLEAN`.

- [ ] **Step 3: Update knowledge base**

`web/AGENTS.md`: reflect new module map (`features/dashboard/` with metrics/queries/components), rename, playground captured-token behavior. Root `AGENTS.md`: update the SPA row in WHERE TO LOOK and any STRUCTURE tree references.

- [ ] **Step 4: Final commit**

```bash
git add AGENTS.md web/AGENTS.md
git commit -m "docs(web): update knowledge base for web/ redesign"
```

---

## Self-Review

- **Spec coverage:** §1 IA → Tasks 6 (routes/nav), 7 (focus), 8 (log params). §2 Dashboard → Tasks 2–6 (KPI thresholds 10%/25% in KpiStrip; four queries; URL window param). §3 Panels → Tasks 7–9 (playground picker uses client-captured tokens only — masked-list constraint honored). §4 Structure/rename → Task 1 atomic; feature additions Tasks 2–9. §5 Visual → Task 10. Testing → per-task gates + Task 11 smoke.
- **Placeholder scan:** KpiStrip's dead `topSpenders` block and the duplicated `background` declaration are explicitly flagged for removal in their own steps; no TBD/TODO remain. Query-option import names in Task 6 are flagged for adaptation to actual exports rather than invented.
- **Type consistency:** `WindowTotals`/`PerDaySeries` defined Task 2, consumed Tasks 3–4 verbatim; `SpendKeyRow`/`SpendServiceRow` Task 2 → Task 5; `CapturedToken` Task 9 self-contained; `latencySummary` Task 2 → Task 8.
