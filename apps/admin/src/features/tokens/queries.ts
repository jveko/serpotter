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
  localStorage.setItem(PLAY_TOKEN_KEY, token);
  window.dispatchEvent(new Event("serpotter:play-token"));
}
