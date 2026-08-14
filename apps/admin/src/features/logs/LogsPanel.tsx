import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { usePublishPanelStatus } from "@/features/shell/panel-status";

import {
  FilterDebouncer,
  nextPage,
  prevPage,
  resetToFirstPage,
  requestLogsQueryOptions,
  withFilter,
} from "./queries";
import type { FilterKey } from "./queries";
import type { RequestLogFilters, RequestLogRow } from "./types";

const LIMIT_OPTIONS = [25, 50, 100, 200] as const;
/** Numeric-keyed filters get a numeric soft keyboard on mobile (hint only). */
const NUMERIC_FILTERS: Partial<Record<(typeof FILTER_FIELDS)[number]["key"], "numeric">> = {
  status: "numeric",
};
const FILTER_FIELDS = [
  { key: "path", label: "Path prefix", placeholder: "/api/se" },
  { key: "status", label: "Status", placeholder: "200" },
  { key: "service", label: "Service", placeholder: "firecrawl" },
  { key: "requestId", label: "Request ID", placeholder: "req-…" },
  { key: "tokenName", label: "Token name", placeholder: "tok-" },
] as const;

/**
 * Request logs panel. GET /api/request-logs with server-side filters via
 * TanStack Query. The page head's Refresh invalidates this panel's key
 * (qk.requestLogs.all). Typing updates the `draft` immediately (snappy
 * inputs); the query key only advances after a 300ms quiet window, so
 * per-keystroke fetches collapse into one. Pagination (Prev/Next) commits
 * immediately and resets to page 0 whenever a filter or limit changes.
 */
export function LogsPanel() {
  const [draft, setDraft] = useState<RequestLogFilters>({ limit: 50 });
  const [filters, setFilters] = useState<RequestLogFilters>({ limit: 50 });
  const debouncerRef = useRef<FilterDebouncer | null>(null);
  function getDebouncer(): FilterDebouncer {
    if (!debouncerRef.current) {
      debouncerRef.current = new FilterDebouncer((f) => setFilters(f));
    }
    return debouncerRef.current;
  }
  useEffect(() => () => getDebouncer().cancel(), []);
  const { data, error, isPending, isFetching, refetch } = useQuery(
    requestLogsQueryOptions(filters),
  );

  const logs: RequestLogRow[] = Array.isArray(data) ? data : [];
  const offset = filters.offset ?? 0;
  const atFirstPage = offset === 0;
  const atLastPage = logs.length < filters.limit;

  const updateFilter = (key: FilterKey) => (value: string) => {
    const next = resetToFirstPage(withFilter(draft, key, value));
    setDraft(next);
    getDebouncer().push(next);
  };

  const changeLimit = (limit: number) => {
    const next = resetToFirstPage({ ...draft, limit });
    setDraft(next);
    getDebouncer().push(next);
  };

  const goToPage = (apply: (f: RequestLogFilters) => RequestLogFilters) => {
    const next = apply(draft);
    setDraft(next);
    // Pagination is a discrete action — commit immediately, no debounce.
    setFilters(next);
  };

  const errMsg = error instanceof Error ? error.message : error ? String(error) : null;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (isFetching) state = "refreshing";

  usePublishPanelStatus(
    state,
    data
      ? `${offset + 1}–${offset + logs.length} · ${logs.length} entries`
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
          Newest first from <span className="mono">/api/request-logs</span>, filtered server-side
          (path prefix; exact status / service / requestId / tokenName), paged with offset. Recent
          2,048 requests are kept in memory — full history lives in the server JSON logs
          (LOG_FORMAT=json).
        </p>
      </div>
      {errMsg ? (
        <p className="err" role="alert">
          {errMsg}
        </p>
      ) : null}
      <div className="row">
        {FILTER_FIELDS.map(({ key, label, placeholder }) => (
          <label key={key} className="field">
            <span className="field__label">{label}</span>
            <input
              className="input"
              value={draft[key] ?? ""}
              onChange={(e) => updateFilter(key)(e.target.value)}
              placeholder={placeholder}
              inputMode={NUMERIC_FILTERS[key]}
            />
          </label>
        ))}
        <label className="field">
          <span className="field__label">Limit</span>
          <select
            className="input"
            value={draft.limit}
            onChange={(e) => changeLimit(Number(e.target.value))}
          >
            {LIMIT_OPTIONS.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="row">
        <span className="field__label">
          {logs.length === 0
            ? "No rows"
            : `Page offset ${offset} (rows ${offset + 1}–${offset + logs.length})`}
        </span>
        <button
          type="button"
          className="btn btn--secondary btn--sm"
          disabled={atFirstPage || isFetching}
          onClick={() => goToPage(prevPage)}
        >
          ← Prev
        </button>
        <button
          type="button"
          className="btn btn--secondary btn--sm"
          disabled={atLastPage || isFetching}
          onClick={() => goToPage(nextPage)}
        >
          Next →
        </button>
      </div>
      <div className="table-scroll bleed">
        <table className="table">
          <thead>
            <tr>
              <th>id</th>
              <th>requestId</th>
              <th>createdAt</th>
              <th>path</th>
              <th>method</th>
              <th>status</th>
              <th>service</th>
              <th>tokenName</th>
              <th>strategy</th>
              <th>attemptCount</th>
              <th>keyId</th>
              <th>nodeId</th>
              <th>providerUsed</th>
              <th>providersConsulted</th>
              <th>durationMs</th>
              <th>errorKind</th>
              <th>queryPreview</th>
            </tr>
          </thead>
          <tbody>
            {logs.length === 0 ? (
              <tr>
                <td colSpan={17} className="empty">
                  No logs
                </td>
              </tr>
            ) : (
              logs.map((r) => (
                <tr key={r.id}>
                  <td>{r.id}</td>
                  <td className="mono">{r.requestId || "—"}</td>
                  <td className="mono">{r.createdAt}</td>
                  <td className="mono">{r.path}</td>
                  <td className="mono">{r.method || "—"}</td>
                  <td>{r.status}</td>
                  <td>{r.service || "—"}</td>
                  <td className="mono">{r.tokenName || "—"}</td>
                  <td className="mono">{r.strategy || "—"}</td>
                  <td>{r.attemptCount ?? "—"}</td>
                  <td>{r.keyId ?? "—"}</td>
                  <td>{r.nodeId ?? "—"}</td>
                  <td>{r.providerUsed || "—"}</td>
                  <td className="mono" title={r.providersConsulted ?? undefined}>
                    {r.providersConsulted?.split(",").join(" · ") || "—"}
                  </td>
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
