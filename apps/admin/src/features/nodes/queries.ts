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
  protocol?: string;
  username?: string;
  password?: string;
}): Promise<unknown> {
  const body: Record<string, unknown> = {
    host: String(p.host ?? "").trim(),
    port: Number(p.port),
    protocol: (p.protocol ?? "http").trim() || "http",
  };
  const user = p.username != null ? String(p.username).trim() : "";
  if (user) body.username = user;
  if (p.password) body.password = p.password;
  return adminFetch("/api/nodes", {
    method: "POST",
    body: JSON.stringify(body),
  });
}

/**
 * PUT /api/nodes/{id} — patch connection settings. `username`/`password` are
 * tri-state: absent = keep, `null` = clear, string = set.
 */
export async function updateNodeRequest(
  id: string | number,
  p: {
    host?: string;
    port?: number;
    protocol?: string;
    username?: string | null;
    password?: string | null;
  },
): Promise<unknown> {
  const body: Record<string, unknown> = {};
  if (p.host != null) body.host = p.host;
  if (p.port != null) body.port = p.port;
  if (p.protocol != null) body.protocol = p.protocol;
  if (p.username !== undefined) body.username = p.username;
  if (p.password !== undefined) body.password = p.password;
  return adminFetch(`/api/nodes/${id}`, {
    method: "PUT",
    body: JSON.stringify(body),
  });
}

export async function toggleNodeRequest(id: string | number): Promise<unknown> {
  return adminFetch(`/api/nodes/${id}/toggle`, { method: "POST" });
}

export async function deleteNodeRequest(id: string | number): Promise<void> {
  await adminFetch(`/api/nodes/${id}`, { method: "DELETE" });
}
