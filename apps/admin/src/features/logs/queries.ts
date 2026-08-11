import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { RequestLogFilters, RequestLogRow } from "./types";

/** Filterable request-log fields (blank values are dropped). */
export type FilterKey = "path" | "status" | "service" | "requestId";

/** Serialize filters to /api/request-logs query params; blank filters are skipped. */
export function buildRequestLogsUrl(f: RequestLogFilters): string {
  const params = new URLSearchParams({ limit: String(f.limit) });
  if (f.status?.trim()) params.set("status", f.status.trim());
  if (f.path?.trim()) params.set("path", f.path.trim());
  if (f.service?.trim()) params.set("service", f.service.trim());
  if (f.requestId?.trim()) params.set("requestId", f.requestId.trim());
  return `/api/request-logs?${params.toString()}`;
}

/** Returns a copy of `f` with one filter field set; blank values remove the field. */
export function withFilter(f: RequestLogFilters, key: FilterKey, value: string): RequestLogFilters {
  const next = { ...f };
  const v = value.trim();
  if (v) next[key] = v;
  else delete next[key];
  return next;
}

/**
 * Debounced commit of typed filter drafts: only the last draft within
 * `delayMs` reaches `commit`, so rapid keystrokes collapse into one fetch
 * instead of one GET /api/request-logs per character.
 */
export class FilterDebouncer {
  private timer: number | null = null;

  constructor(
    private readonly commit: (f: RequestLogFilters) => void,
    private readonly delayMs = 300,
  ) {}

  push(draft: RequestLogFilters): void {
    if (this.timer !== null) window.clearTimeout(this.timer);
    this.timer = window.setTimeout(() => {
      this.timer = null;
      this.commit(draft);
    }, this.delayMs);
  }

  cancel(): void {
    if (this.timer !== null) {
      window.clearTimeout(this.timer);
      this.timer = null;
    }
  }
}

export function requestLogsQueryOptions(filters: RequestLogFilters) {
  return queryOptions({
    queryKey: qk.requestLogs.list(filters),
    queryFn: async () => {
      const logs = await adminFetch<RequestLogRow[]>(buildRequestLogsUrl(filters));
      return Array.isArray(logs) ? logs : [];
    },
    staleTime: 0,
  });
}