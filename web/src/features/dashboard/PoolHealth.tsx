import { Link } from "@tanstack/react-router";

import type { KeyRow } from "@/features/keys/types";
import type { NodeRow } from "@/features/nodes/types";
import type { StatsDto } from "@/features/stats/types";

function CreditBar({
  remaining,
  limit,
}: {
  remaining: number | null | undefined;
  limit: number | null | undefined;
}) {
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

export function PoolHealth({
  stats,
  keys,
  nodes,
}: {
  stats: StatsDto;
  keys: KeyRow[];
  nodes: NodeRow[];
}) {
  const failingKeys = keys.filter((k) => k.consecutiveFails > 0).length;
  const badNodes = nodes.filter((n) => !n.enabled || n.lastError != null).length;
  return (
    <div className="pool-health">
      {stats.byService.map((s) => (
        <div key={s.service} className="pool-health__svc">
          <span className="lb-label">{s.service}</span>
          <CreditBar remaining={s.creditsRemaining} limit={s.creditsLimit} />
          <span className="num pool-health__count">
            {s.active}/{s.keys}
          </span>
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
