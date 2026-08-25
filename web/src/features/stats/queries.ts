import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { StatsDto, UsageDailyDto } from "./types";

export const statsQueryOptions = queryOptions({
  queryKey: qk.stats.summary(),
  queryFn: () => adminFetch<StatsDto>("/api/stats"),
  staleTime: 10_000,
});

/** Daily usage for the Stats usage table; days clamped 1..=180 server-side. */
export function usageQueryOptions(days: number) {
  return queryOptions({
    queryKey: qk.stats.usage(days),
    queryFn: () => adminFetch<UsageDailyDto[]>(`/api/usage?days=${days}`),
    staleTime: 30_000,
  });
}
