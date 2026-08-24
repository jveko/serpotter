import type { QueryClient } from "@tanstack/react-query";

import { broadcastAuthCleared, clearAuthStorage } from "@/features/auth/session-end";
import { router } from "@/router";

/** 401 path: clear auth storage + React via event, drop query cache, go login. */
export function endAdminSession(queryClient: QueryClient): void {
  // clearAuthStorage also syncs auth snapshot so beforeLoad sees unauthenticated same-turn;
  // the key removals fire `storage` events that reset every other open tab.
  clearAuthStorage();
  queryClient.clear();
  broadcastAuthCleared();
  void router.navigate({ to: "/login", search: { redirect: undefined } });
  void router.invalidate();
}
