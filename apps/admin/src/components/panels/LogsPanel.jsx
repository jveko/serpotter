import React from "react";

/**
 * Request logs panel. Presentational table + refresh callback.
 */
export function LogsPanel({ requestLogs = [], busy, onRefresh }) {
  return (
    <section className="panel" id="logs">
      <div className="panel__head">
        <h2 className="panel__title">Request logs</h2>
        <div className="panel__meta">
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
          Latest 50 from GET /api/request-logs (newest first).
        </p>
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr>
                <th>id</th>
                <th>createdAt</th>
                <th>path</th>
                <th>status</th>
                <th>service</th>
                <th>providerUsed</th>
                <th>durationMs</th>
                <th>errorKind</th>
                <th>queryPreview</th>
              </tr>
            </thead>
            <tbody>
              {requestLogs.length === 0 ? (
                <tr>
                  <td colSpan={9} className="empty">
                    No logs
                  </td>
                </tr>
              ) : (
                requestLogs.map((r) => (
                  <tr key={r.id}>
                    <td>{r.id}</td>
                    <td className="mono">{r.createdAt}</td>
                    <td className="mono">{r.path}</td>
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
