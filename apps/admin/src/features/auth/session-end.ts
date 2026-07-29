import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

import { syncAuthSnapshotFromStorage } from "./auth-snapshot";

/** Clears admin identity only — never PLAY_TOKEN_KEY. */
export function clearAuthStorage(): void {
  localStorage.removeItem(SECRET_KEY);
  localStorage.removeItem(SESSION_KEY);
  localStorage.removeItem(SESSION_EXPIRES_KEY);
  syncAuthSnapshotFromStorage();
}
