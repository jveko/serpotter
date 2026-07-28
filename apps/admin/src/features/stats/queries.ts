import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { StatsDto } from "./types";

export const statsQueryOptions = queryOptions({
  queryKey: qk.stats.summary(),
  queryFn: () => adminFetch<StatsDto>("/api/stats"),
  staleTime: 10_000,
});
