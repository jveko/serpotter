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
