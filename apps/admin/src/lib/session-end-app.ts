import type { QueryClient } from "@tanstack/react-query";

import { clearAuthStorage } from "@/features/auth/session-end";
import { router } from "@/router";

/** 401 path: clear auth storage + React via event, drop query cache, go login. */
export function endAdminSession(queryClient: QueryClient): void {
  // clearAuthStorage also syncs auth snapshot so beforeLoad sees unauthenticated same-turn.
  clearAuthStorage();
  queryClient.clear();
  window.dispatchEvent(new Event("serpotter:auth-cleared"));
  void router.navigate({ to: "/login", search: { redirect: undefined } });
  void router.invalidate();
}
