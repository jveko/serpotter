import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { latencySummary } from "@/features/dashboard/metrics";
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
import { RowDetail } from "./RowDetail";
import type { RequestLogFilters, RequestLogRow } from "./types";

const LIMIT_OPTIONS = [25, 50, 100, 200] as const;

/** Status-class chips: a single digit class ("2" | "4" | "5") or "" for all.
 * The server matches status exactly (as i64), so classes are filtered
 * client-side over the loaded ring window; the exact-status text input below
 * still hits the server param. */
const STATUS_CLASSES: Array<{ value: "" | "2" | "4" | "5"; label: string }> = [
  { value: "", label: "all" },
  { value: "2", label: "2xx" },
  { value: "4", label: "4xx" },
  { value: "5", label: "5xx" },
];

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

const COL_COUNT = 13;

/**
 * Request logs panel. GET /api/request-logs with server-side filters via
 * TanStack Query. The page head's Refresh invalidates this panel's key
 * (qk.requestLogs.all). Typing updates the `draft` immediately (snappy
 * inputs); the query key only advances after a 300ms quiet window, so
 * per-keystroke fetches collapse into one. Pagination (Prev/Next) commits
 * immediately and resets to page 0 whenever a filter or limit changes.
 *
 * `initialRequestId` / `initialStatus` seed the panel from the route search
 * (Task 5 deep links), so /logs?requestId=… & /logs?status=4 land filtered.
 * Status classes (2xx/4xx/5xx) filter the loaded ring window client-side
 * because the server matches status exactly.
 */
export function LogsPanel({
  initialRequestId,
  initialStatus,
}: { initialRequestId?: string; initialStatus?: string } = {}) {
  const [draft, setDraft] = useState<RequestLogFilters>({ limit: 50 });
  const [filters, setFilters] = useState<RequestLogFilters>({ limit: 50 });
  const [statusClass, setStatusClass] = useState<"" | "2" | "4" | "5">("");
  const [expandedId, setExpandedId] = useState<number | null>(null);
  const debouncerRef = useRef<FilterDebouncer | null>(null);
  function getDebouncer(): FilterDebouncer {
    if (!debouncerRef.current) {
      debouncerRef.current = new FilterDebouncer((f) => setFilters(f));
    }
    return debouncerRef.current;
  }
  useEffect(() => () => getDebouncer().cancel(), []);

  // Seed from route search: requestId is an exact server filter; status is a
  // client-side class. Re-derives cleanly whenever the URL params change.
  useEffect(() => {
    getDebouncer().cancel();
    setDraft((prev) => {
      const next: RequestLogFilters = { limit: prev.limit };
      if (initialRequestId) next.requestId = initialRequestId;
      return next;
    });
    setFilters((prev) => {
      const next: RequestLogFilters = { limit: prev.limit };
      if (initialRequestId) next.requestId = initialRequestId;
      return next;
    });
    // The route already validates status to /^[245]$/; guard here for TS.
    const seededStatus: "" | "2" | "4" | "5" =
      initialStatus === "2" || initialStatus === "4" || initialStatus === "5" ? initialStatus : "";
    setStatusClass(seededStatus);
  }, [initialRequestId, initialStatus]);

  const { data, error, isPending, isFetching, refetch } = useQuery(
    requestLogsQueryOptions(filters),
  );

  const logs: RequestLogRow[] = Array.isArray(data) ? data : [];
  const visibleLogs =
    statusClass === ""
      ? logs
      : logs.filter((r) => Math.floor(r.status / 100).toString() === statusClass);
  const { p50, p95 } = latencySummary(visibleLogs);
  const offset = filters.offset ?? 0;
  const atFirstPage = offset === 0;
  const atLastPage = logs.length < filters.limit;

  const updateFilter = (key: FilterKey) => (value: string) => {
    const next = resetToFirstPage(withFilter(draft, key, value));
    setDraft(next);
    getDebouncer().push(next);
  };

  const setStatusClassChip = (value: "" | "2" | "4" | "5") => () => {
    setExpandedId(null);
    setStatusClass(value);
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
    data ? `${offset + 1}–${offset + logs.length} · ${logs.length} entries` : undefined,
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
        <p className="block__note">
          p50 {p50 ?? "—"}ms · p95 {p95 ?? "—"}ms <span className="mono">(ring window)</span>
        </p>
      </div>
      {errMsg ? (
        <p className="err" role="alert">
          {errMsg}
        </p>
      ) : null}
      <div className="row" role="group" aria-label="Filter by status class">
        {STATUS_CLASSES.map(({ value, label }) => (
          <button
            key={value}
            type="button"
            className={`chip ${statusClass === value ? "chip--live" : ""}`}
            aria-pressed={statusClass === value}
            onClick={setStatusClassChip(value)}
          >
            {label}
          </button>
        ))}
      </div>
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
          {visibleLogs.length === 0
            ? "No rows"
            : `Page offset ${offset} (rows ${offset + 1}–${offset + visibleLogs.length})`}
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
              <th>providerUsed</th>
              <th>durationMs</th>
              <th>errorKind</th>
              <th>queryPreview</th>
              <th aria-label="Toggle row details" />
            </tr>
          </thead>
          <tbody>
            {visibleLogs.length === 0 ? (
              <tr>
                <td colSpan={COL_COUNT} className="empty">
                  No logs
                </td>
              </tr>
            ) : (
              visibleLogs.map((r) => (
                <FragmentRow
                  key={r.id}
                  row={r}
                  expanded={expandedId === r.id}
                  onToggle={() => setExpandedId(expandedId === r.id ? null : r.id)}
                />
              ))
            )}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function FragmentRow({
  row: r,
  expanded,
  onToggle,
}: {
  row: RequestLogRow;
  expanded: boolean;
  onToggle: () => void;
}) {
  return (
    <>
      <tr>
        <td className="num">{r.id}</td>
        <td className="mono">{r.requestId || "—"}</td>
        <td className="mono">{r.createdAt}</td>
        <td className="mono">{r.path}</td>
        <td className="mono">{r.method || "—"}</td>
        <td className="num">{r.status}</td>
        <td>{r.service || "—"}</td>
        <td className="mono">{r.tokenName || "—"}</td>
        <td>{r.providerUsed || "—"}</td>
        <td className="num">{r.durationMs ?? "—"}</td>
        <td className="mono">{r.errorKind || "—"}</td>
        <td className="mono break">{r.queryPreview || "—"}</td>
        <td>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            aria-expanded={expanded}
            aria-label={expanded ? "Collapse row details" : "Expand row details"}
            onClick={onToggle}
          >
            {expanded ? "▾" : "▸"}
          </button>
        </td>
      </tr>
      {expanded ? (
        <tr>
          <td colSpan={COL_COUNT} className="row-detail__cell">
            <RowDetail row={r} />
          </td>
        </tr>
      ) : null}
    </>
  );
}
