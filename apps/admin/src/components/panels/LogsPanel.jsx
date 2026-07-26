import React, { useMemo, useState } from "react";

/**
 * Request logs panel. Client-side method filter (API is limit-only).
 */
export function LogsPanel({ requestLogs = [], busy, onRefresh }) {
  const [methodFilter, setMethodFilter] = useState("");
  const methods = useMemo(() => {
    const set = new Set();
    for (const r of requestLogs) {
      if (r?.method) set.add(String(r.method));
    }
    return Array.from(set).sort();
  }, [requestLogs]);
  const filtered = useMemo(() => {
    if (!methodFilter) return requestLogs;
    return requestLogs.filter(
      (r) => String(r?.method || "") === methodFilter,
    );
  }, [requestLogs, methodFilter]);

  return (
    <section className="panel" id="logs">
      <div className="panel__head">
        <h2 className="panel__title">Request logs</h2>
        <div className="panel__meta">
          <label className="field" style={{ margin: 0 }}>
            <span className="field__label">Method</span>
            <select
              className="field__control"
              value={methodFilter}
              disabled={busy}
              onChange={(e) => setMethodFilter(e.target.value)}
            >
              <option value="">All</option>
              {methods.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            disabled={busy}
            data-state={busy ? "loading" : undefined}
            onClick={onRefresh}
          >
            Refresh logs
          </button>
        </div>
      </div>
      <div className="panel__body">
        <p className="panel__lede">
          Latest 50 from GET /api/request-logs (newest first)
          {methodFilter
            ? `; showing ${filtered.length} with method=${methodFilter}`
            : ""}
          . Filter is client-side only.
        </p>
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr>
                <th>id</th>
                <th>createdAt</th>
                <th>path</th>
                <th>method</th>
                <th>status</th>
                <th>service</th>
                <th>providerUsed</th>
                <th>durationMs</th>
                <th>errorKind</th>
                <th>queryPreview</th>
              </tr>
            </thead>
            <tbody>
              {filtered.length === 0 ? (
                <tr>
                  <td colSpan={10} className="empty">
                    No logs
                  </td>
                </tr>
              ) : (
                filtered.map((r) => (
                  <tr key={r.id}>
                    <td>{r.id}</td>
                    <td className="mono">{r.createdAt}</td>
                    <td className="mono">{r.path}</td>
                    <td className="mono">{r.method || "—"}</td>
                    <td>{r.status}</td>
                    <td>{r.service || "—"}</td>
                    <td>{r.providerUsed || "—"}</td>
                    <td>{r.durationMs ?? "—"}</td>
                    <td className="mono">{r.errorKind || "—"}</td>
                    <td className="mono break">{r.queryPreview || "—"}</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </section>
  );
}
