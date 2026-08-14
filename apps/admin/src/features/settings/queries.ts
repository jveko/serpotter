import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { AdminSessionDto, SettingsDto } from "./types";

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

/** Password-rotation client-side policy mirroring the API (min 8 chars). */
export function passwordPolicyError(newPassword: string): string | null {
  if (newPassword.trim().length < 8) {
    return "New password must be at least 8 characters";
  }
  return null;
}

/** POST /api/admin/change-password → { ok: true }; throws HttpError on 4xx/5xx. */
export async function changePasswordRequest(
  currentPassword: string,
  newPassword: string,
): Promise<{ ok: boolean }> {
  return adminFetch<{ ok: boolean }>("/api/admin/change-password", {
    method: "POST",
    body: JSON.stringify({ currentPassword, newPassword }),
  });
}

/** GET /api/admin/sessions — active admin sessions, newest first. */
export const adminSessionsQueryOptions = queryOptions({
  queryKey: qk.admin.sessions(),
  queryFn: () => adminFetch<AdminSessionDto[]>("/api/admin/sessions"),
  staleTime: 15_000,
});

/** DELETE /api/admin/sessions/{token} — revoke one session (204). */
export async function revokeAdminSessionRequest(token: string): Promise<void> {
  await adminFetch(`/api/admin/sessions/${encodeURIComponent(token)}`, { method: "DELETE" });
}
