import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { RequestLogRow } from "./types";

export const requestLogsQueryOptions = queryOptions({
  queryKey: qk.requestLogs.list({ limit: 50 }),
  queryFn: async () => {
    const logs = await adminFetch<RequestLogRow[]>("/api/request-logs?limit=50");
    return Array.isArray(logs) ? logs : [];
  },
  staleTime: 0,
});
