import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { RequestLogFilters, RequestLogRow } from "./types";

/** Serialize filters to /api/request-logs query params; blank filters are skipped. */
function buildRequestLogsUrl(f: RequestLogFilters): string {
  const params = new URLSearchParams({ limit: String(f.limit) });
  if (f.status?.trim()) params.set("status", f.status.trim());
  if (f.path?.trim()) params.set("path", f.path.trim());
  if (f.service?.trim()) params.set("service", f.service.trim());
  if (f.requestId?.trim()) params.set("requestId", f.requestId.trim());
  return `/api/request-logs?${params.toString()}`;
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
