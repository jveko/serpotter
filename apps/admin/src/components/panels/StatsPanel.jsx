import React from "react";

/**
 * Stats inventory panel: strip metrics + byService table.
 * Presentational only — no mutations.
 */
export function StatsPanel({ stats }) {
  const byService = Array.isArray(stats?.byService) ? stats.byService : [];

  return (
    <section className="panel" id="stats">
      <div className="panel__head">
        <h2 className="panel__title">Stats</h2>
        <span className="panel__meta">{stats ? "live" : "loading"}</span>
      </div>
      <div className="panel__body">
        {stats ? (
          <>
            <div className="stat-strip">
              <div className="stat">
                <span className="stat__label">tokens</span>
                <span className="stat__value">{stats.tokens}</span>
              </div>
              <div className="stat">
                <span className="stat__label">keys</span>
                <span className="stat__value">{stats.apiKeys}</span>
              </div>
              <div className="stat">
                <span className="stat__label">active</span>
                <span className="stat__value">{stats.activeApiKeys}</span>
              </div>
              <div className="stat">
                <span className="stat__label">nodes</span>
                <span className="stat__value">{stats.nodes}</span>
              </div>
              <div className="stat">
                <span className="stat__label">schema</span>
                <span className="stat__value">{stats.schemaVersion}</span>
              </div>
              <div className="stat">
                <span className="stat__label">requestLogs</span>
                <span className="stat__value">{stats.requestLogs ?? 0}</span>
              </div>
            </div>
            {byService.length > 0 && (
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>service</th>
                      <th>keys</th>
                      <th>active</th>
                      <th>creditsRemaining</th>
                      <th>creditsLimit</th>
                    </tr>
                  </thead>
                  <tbody>
                    {byService.map((s) => (
                      <tr key={s.service}>
                        <td>{s.service}</td>
                        <td>{s.keys}</td>
                        <td>{s.active}</td>
                        <td className="mono">{s.creditsRemaining ?? "—"}</td>
                        <td className="mono">{s.creditsLimit ?? "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        ) : (
          <p className="empty">Loading…</p>
        )}
      </div>
    </section>
  );
}
