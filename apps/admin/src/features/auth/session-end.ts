import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

import { syncAuthSnapshotFromStorage } from "./auth-snapshot";

/**
 * Same-tab broadcast consumed by AuthProvider. Other tabs are NOT notified by
 * window.dispatchEvent — they sync via the `storage` event fired by the
 * clearAuthStorage removals (onAuthStorageChanged in auth-snapshot.ts).
 */
export function broadcastAuthCleared(): void {
  window.dispatchEvent(new Event("serpotter:auth-cleared"));
}

/** Clears admin identity only — never PLAY_TOKEN_KEY. */
export function clearAuthStorage(): void {
  localStorage.removeItem(SECRET_KEY);
  localStorage.removeItem(SESSION_KEY);
  localStorage.removeItem(SESSION_EXPIRES_KEY);
  syncAuthSnapshotFromStorage();
}
