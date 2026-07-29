import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { requestLogsQueryOptions } from "./queries";
import type { RequestLogRow } from "./types";

/**
 * Request logs panel. GET /api/request-logs?limit=50 via TanStack Query.
 * Refresh = refetch only. Client filter on path/method/status substring.
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

  let meta = "live";
  if (isPending && !data) meta = "loading";
  else if (error && !data) meta = "error";
  else if (isFetching) meta = "refreshing";

  const busy = isFetching;

  return (
    <section className="panel" id="logs">
      <div className="panel__head">
        <h2 className="panel__title">Request logs</h2>
        <div className="panel__meta">
          <span>{meta}</span>
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            disabled={busy && !data}
            data-state={isFetching ? "loading" : undefined}
            onClick={() => void refetch()}
          >
            Refresh logs
          </button>
        </div>
      </div>
      <div className="panel__body">
        {isPending && !data ? (
          <p className="empty" aria-busy="true">
            Loading…
          </p>
        ) : error && !data ? (
          <div className="banner" role="alert">
            <p className="banner__text err">{errMsg}</p>
            <button
              type="button"
              className="btn btn--secondary btn--sm"
              onClick={() => void refetch()}
            >
              Retry
            </button>
          </div>
        ) : (
          <>
            <p className="panel__lede">
              Latest 50 from GET /api/request-logs (newest first)
              {q ? `; showing ${visible.length} matching “${filter.trim()}”` : ""}. Filter is
              client-side only.
            </p>
            {errMsg && data ? (
              <p className="banner__text err" role="alert">
                {errMsg}
              </p>
            ) : null}
            <label className="field">
              <span className="field__label">Filter</span>
              <input
                className="input"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="path, method, status"
                disabled={isPending && !data}
              />
            </label>
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
          </>
        )}
      </div>
    </section>
  );
}
