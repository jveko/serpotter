import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { SettingsDto } from "./types";

export const settingsQueryOptions = queryOptions({
  queryKey: qk.settings.root(),
  queryFn: () => adminFetch<SettingsDto>("/api/settings"),
  staleTime: 60_000,
});

/**
 * Re-sync decision for the SettingsPanel socialEnabled draft. Once the user
 * has touched the toggle, a refetch (page-head Refresh, window-focus refetch)
 * must not clobber the unsaved value: a dirty tab keeps its draft until save
 * or until the user toggles back; untouched tabs adopt the server value.
 */
export function reconcileSocialDraft(current: boolean, saved: boolean, touched: boolean): boolean {
  if (touched && current !== saved) return current;
  return saved;
}