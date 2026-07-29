import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import type { NodeRow } from "./types";

export const nodesQueryOptions = queryOptions({
  queryKey: qk.nodes.list(),
  queryFn: () => adminFetch<NodeRow[]>("/api/nodes"),
  staleTime: 10_000,
});

/** Create node — omit empty username like useAdminData. */
export async function createNodeRequest(p: {
  host: string;
  port: number | string;
  username?: string;
  password?: string;
}): Promise<unknown> {
  const body: Record<string, unknown> = {
    host: String(p.host ?? "").trim(),
    port: Number(p.port),
  };
  const user = p.username != null ? String(p.username).trim() : "";
  if (user) body.username = user;
  if (p.password) body.password = p.password;
  return adminFetch("/api/nodes", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export async function toggleNodeRequest(id: string | number): Promise<unknown> {
  return adminFetch(`/api/nodes/${id}/toggle`, { method: "POST" });
}

export async function deleteNodeRequest(id: string | number): Promise<void> {
  await adminFetch(`/api/nodes/${id}`, { method: "DELETE" });
}
