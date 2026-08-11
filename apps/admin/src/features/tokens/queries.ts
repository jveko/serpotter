import type { QueryClient } from "@tanstack/react-query";
import { queryOptions } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { PLAY_TOKEN_KEY } from "@/lib/constants";
import { qk } from "@/lib/query-keys";

import type { CreateTokenResult, TokenRow } from "./types";

export const tokensQueryOptions = queryOptions({
  queryKey: qk.tokens.list(),
  queryFn: () => adminFetch<TokenRow[]>("/api/tokens"),
  staleTime: 10_000,
});

export async function createTokenRequest(p: { name: string }): Promise<CreateTokenResult> {
  return adminFetch<CreateTokenResult>("/api/tokens", {
    method: "POST",
    body: JSON.stringify({ name: p.name }),
  });
}

export async function deleteTokenRequest(id: string | number): Promise<void> {
  await adminFetch(`/api/tokens/${id}`, { method: "DELETE" });
}

/** Sets playground token storage + event; the caller navigates to /playground. */
export function useInPlayground(token: string): void {
  try {
    localStorage.setItem(PLAY_TOKEN_KEY, token);
  } catch {
    // Storage unavailable/quota — the playground still opens; the token is
    // simply not persisted across reloads.
  }
  window.dispatchEvent(new Event("serpotter:play-token"));
}

/**
 * Invalidate the tokens list + stats summary: creating/deleting a token
 * changes the server-side token count that /api/stats reports.
 */
export async function invalidateTokensAndStats(qc: QueryClient): Promise<void> {
  await Promise.all([
    qc.invalidateQueries({ queryKey: qk.tokens.all }),
    qc.invalidateQueries({ queryKey: qk.stats.all }),
  ]);
}

/**
 * Clear the persisted playground token when the deleted token's raw value
 * matches it. Only the just-created token's full value is known client-side
 * (server rows expose a masked preview), so that is the only matchable token.
 * Returns true when the key was removed.
 */
export function maybeClearPlayToken(
  deletedId: string | number,
  created: { id: string | number; token: string } | null,
): boolean {
  if (!created || String(deletedId) !== String(created.id)) return false;
  if (localStorage.getItem(PLAY_TOKEN_KEY) !== created.token) return false;
  localStorage.removeItem(PLAY_TOKEN_KEY);
  return true;
}
