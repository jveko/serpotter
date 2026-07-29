import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { usePublishPanelStatus } from "@/features/shell/panel-status";

import { requestLogsQueryOptions } from "./queries";
import type { RequestLogRow } from "./types";

/**
 * Request logs panel. GET /api/request-logs?limit=50 via TanStack Query.
 * The page head's Refresh invalidates this panel's key. Client filter on
 * path/method/status substring.
 */
export function LogsPanel() {
  const { data, error, isPending, isFetching, refetch } = useQuery(requestLogsQueryOptions);
  const [filter, setFilter] = useState("");

  const logs: RequestLogRow[] = Array.isArray(data) ? data : [];
  const q = filter.trim().toLowerCase();
  const visible = useMemo(
    () =>
      q
        ? logs.filter(
            (r) =>
              (r.path || "").toLowerCase().includes(q) ||
              (r.method || "").toLowerCase().includes(q) ||
              String(r.status).includes(q),
          )
        : logs,
    [logs, q],
  );

  const errMsg = error instanceof Error ? error.message : error ? String(error) : null;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (isFetching) state = "refreshing";

  usePublishPanelStatus(
    state,
    data
      ? q
        ? `${visible.length} of ${logs.length} entries`
        : `${logs.length} entries`
      : undefined,
  );

  if (isPending && !data) {
    return (
      <p className="empty" aria-busy="true">
        Loading…
      </p>
    );
  }

  if (error && !data) {
    return (
      <div className="block">
        <p className="err" role="alert">
          {errMsg}
        </p>
        <div className="row">
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            onClick={() => void refetch()}
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  return (
    <section className="block" id="logs" aria-labelledby="logs-recent">
      <div className="block__head">
        <h2 className="block__title" id="logs-recent">
          Recent requests
        </h2>
        <p className="block__note">
          The latest 50 rows from <span className="mono">/api/request-logs</span>, newest first.
          Filtering is client-side over the loaded rows only.
        </p>
      </div>
      {errMsg ? (
        <p className="err" role="alert">
          {errMsg}
        </p>
      ) : null}
      <div className="row">
        <label className="field">
          <span className="field__label">Filter</span>
          <input
            className="input"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="path, method, status"
          />
        </label>
      </div>
      <div className="table-scroll bleed">
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
            {visible.length === 0 ? (
              <tr>
                <td colSpan={10} className="empty">
                  No logs
                </td>
              </tr>
            ) : (
              visible.map((r) => (
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
    </section>
  );
}
