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