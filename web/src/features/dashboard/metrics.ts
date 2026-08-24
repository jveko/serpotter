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
 * Split usage rows into the most-recent `days` window and the window before
 * it, by ISO date string comparison (dates are YYYY-MM-DD). The caller
 * fetches roughly 2×`days` of usage, so every row strictly before the cutoff
 * belongs to `previous`.
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
    previous: rows.filter((r) => r.date <= cutoffIso),
  };
}

export function windowTotals(rows: UsageDailyDto[]): WindowTotals {
  return rows.reduce<WindowTotals>(
    (acc, r) => ({
      requests: acc.requests + r.requests,
      successes: acc.successes + r.successes,
      errors: acc.errors + r.errors,
      tokens: acc.tokens + r.tokens,
      cost: acc.cost + r.cost,
    }),
    { ...EMPTY_TOTALS },
  );
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
  const durations = rows.map((r) => r.durationMs).filter((d): d is number => typeof d === "number");
  return { p50: percentile(durations, 0.5), p95: percentile(durations, 0.95) };
}
