import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { SettingsDto } from "./types";

export const settingsQueryOptions = queryOptions({
  queryKey: qk.settings.root(),
  queryFn: () => adminFetch<SettingsDto>("/api/settings"),
  staleTime: 60_000,
});
