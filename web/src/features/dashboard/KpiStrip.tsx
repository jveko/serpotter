import type { StatsDto } from "@/features/stats/types";

import { errorRate, type WindowTotals } from "./metrics";

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
    rate == null
      ? ""
      : rate >= 0.25
        ? "kpi__chip is-bad"
        : rate >= 0.1
          ? "kpi__chip is-warn"
          : "kpi__chip";
  const reqDelta = delta(totals.requests, previousTotals?.requests);

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
