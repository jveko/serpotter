import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { SpendKeyRow, SpendServiceRow } from "./types";

export function spendKeysQueryOptions() {
  return queryOptions({
    queryKey: qk.spend.keys(),
    queryFn: () => adminFetch<SpendKeyRow[]>("/api/spend/keys"),
    staleTime: 60_000,
  });
}

export function spendServicesQueryOptions() {
  return queryOptions({
    queryKey: qk.spend.services(),
    queryFn: () => adminFetch<SpendServiceRow[]>("/api/spend/services"),
    staleTime: 60_000,
  });
}