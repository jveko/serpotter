import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { KeyRow, SyncReport } from "./types";

export const keysQueryOptions = queryOptions({
  queryKey: qk.keys.list(),
  queryFn: () => adminFetch<KeyRow[]>("/api/keys"),
  staleTime: 10_000,
});

export async function createKeyRequest(p: { service: string; key: string }): Promise<unknown> {
  return adminFetch("/api/keys", {
    method: "POST",
    body: JSON.stringify({ service: p.service, key: p.key }),
  });
}

export async function toggleKeyRequest(id: string | number): Promise<unknown> {
  return adminFetch(`/api/keys/${id}/toggle`, { method: "POST" });
}

export async function deleteKeyRequest(id: string | number): Promise<void> {
  await adminFetch(`/api/keys/${id}`, { method: "DELETE" });
}

/**
 * Sync credits. Honesty strings match useAdminData.js exactly.
 * Partial (errors>0) throws so mutation.error is set (RQ v5-safe).
 * Clean success returns the notice string.
 */
export async function syncCreditsRequest(p?: { service?: string }): Promise<string> {
  const body: Record<string, string> = {};
  if (p?.service) body.service = p.service;
  const report = await adminFetch<SyncReport>("/api/keys/sync-credits", {
    method: "POST",
    body: JSON.stringify(body),
  });
  const synced = Number(report?.synced ?? 0);
  const errors = Number(report?.errors ?? 0);
  const results = Array.isArray(report?.results) ? report.results : [];
  const failed = results.filter((r) => r && r.ok === false);
  const ok = results.filter((r) => r && r.ok === true);
  const failDetail =
    failed.length > 0
      ? `; failed: ${failed.map((r) => (r.error ? `#${r.id}: ${r.error}` : `#${r.id}`)).join(",")}`
      : "";
  const okDetail =
    ok.length > 0 && errors > 0 ? `; ok: ${ok.map((r) => `#${r.id}`).join(",")}` : "";
  if (errors > 0) {
    throw new Error(
      `Credit sync partial: synced=${synced}, errors=${errors}${failDetail}${okDetail} (exa/xai soft-fail or fetch error; keys stay active)`,
    );
  }
  return `Credit sync: synced=${synced}, errors=0`;
}
