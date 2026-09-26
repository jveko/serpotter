import { describe, expect, it } from "vitest";

import { aggregateRegion, dashboardPanelStatus, errorMessage, failedRegions } from "./status";

function q(over: Partial<Parameters<typeof aggregateRegion>[2][number]> = {}) {
  return { data: {}, error: null, isPending: false, isFetching: false, ...over };
}

const loaded = (id: "usage" | "pool" | "kpi", label: string) => aggregateRegion(id, label, [q()]);
const broken = (id: "kpi" | "usage" | "pool" | "activity" | "leaderboard", label: string) =>
  aggregateRegion(id, label, [q({ data: null, error: new Error(`${label} down`) })]);

describe("aggregateRegion", () => {
  it("needs every feeding query before a card counts as loaded", () => {
    // KpiStrip draws totals from usage, so a usage-only failure is a KPI failure.
    const region = aggregateRegion("kpi", "KPI summary", [q({ data: {} }), q({ data: null })]);
    expect(region.loaded).toBe(false);
  });

  it("surfaces the first error among its sources", () => {
    const region = aggregateRegion("pool", "Pool health", [
      q(),
      q({ data: null, error: new Error("nodes 500") }),
    ]);
    expect(region.loaded).toBe(false);
    expect(errorMessage(region.error)).toBe("nodes 500");
  });
});

describe("failedRegions", () => {
  it("alerts on a stale-but-errored card, matching StatsPanel", () => {
    const ok = loaded("usage", "Usage chart");
    const neverLoaded = broken("kpi", "KPI summary");
    // Refetch failed while old data is still painted: the numbers on screen
    // are no longer known to be current, so this must alert too.
    const stale = aggregateRegion("pool", "Pool health", [
      q({ data: {}, error: new Error("refetch failed"), isFetching: true }),
    ]);
    expect(failedRegions([ok, neverLoaded, stale]).map((r) => r.id)).toEqual(["kpi", "pool"]);
  });

  it("covers a failed request-log feed (recent activity)", () => {
    const regions = [loaded("usage", "Usage chart"), broken("activity", "Recent activity")];
    expect(failedRegions(regions).map((r) => r.id)).toEqual(["activity"]);
    expect(dashboardPanelStatus(regions).state).toBe("error");
  });
});

describe("dashboardPanelStatus", () => {
  it("reports error for a usage-only failure that would zero the KPI strip", () => {
    const status = dashboardPanelStatus([
      aggregateRegion("kpi", "KPI summary", [
        q({ data: {} }),
        q({ data: null, error: new Error("x") }),
      ]),
      loaded("usage", "Usage chart"),
    ]);
    expect(status.state).toBe("error");
  });

  it("stays loading while any card has never loaded, even if one is fetching", () => {
    // First resolution of the fastest query must not read as a background refresh.
    const status = dashboardPanelStatus([
      loaded("usage", "Usage chart"),
      aggregateRegion("kpi", "KPI summary", [q({ data: null, isFetching: true })]),
    ]);
    expect(status.state).toBe("loading");
  });

  it("reports refreshing only when every card has data and one is refetching", () => {
    const status = dashboardPanelStatus([
      loaded("usage", "Usage chart"),
      aggregateRegion("kpi", "KPI summary", [q({ isFetching: true })]),
    ]);
    expect(status.state).toBe("refreshing");
  });

  it("reports live with an honest card count once everything resolved", () => {
    const status = dashboardPanelStatus([loaded("usage", "Usage chart"), loaded("kpi", "KPI")]);
    expect(status).toEqual({ state: "live", detail: "2/2 cards" });
  });
});
