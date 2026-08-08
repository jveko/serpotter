import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

/**
 * Module-level auth gate for route beforeLoad.
 * RouterProvider context.auth only updates on React re-render; logout/401
 * navigate in the same turn would otherwise still see isAuthenticated true.
 * Storage writes + this snapshot stay in lockstep; play token is never touched.
 */
export type AuthSnapshot = {
  token: string;
  sessionExpiresAt: string;
  isAuthenticated: boolean;
};

/** True only when a token exists AND the session window (if any) has not lapsed. */
function isSessionLive(token: string, sessionExpiresAt: string): boolean {
  return Boolean(token) && (!sessionExpiresAt || new Date(sessionExpiresAt).getTime() > Date.now());
}

function readStorage(): AuthSnapshot {
  if (typeof localStorage === "undefined") {
    return { token: "", sessionExpiresAt: "", isAuthenticated: false };
  }
  const token = localStorage.getItem(SESSION_KEY) || localStorage.getItem(SECRET_KEY) || "";
  const sessionExpiresAt = localStorage.getItem(SESSION_EXPIRES_KEY) || "";
  return {
    token,
    sessionExpiresAt,
    isAuthenticated: isSessionLive(token, sessionExpiresAt),
  };
}

let snapshot: AuthSnapshot = readStorage();

export function getAuthSnapshot(): AuthSnapshot {
  // Re-evaluate expiry at read time: the cached snapshot may predate the
  // expiry moment (no storage write happens on lapse — auth-context owns the
  // clear), so route guards never see a stale authenticated claim.
  if (!snapshot.isAuthenticated || !snapshot.sessionExpiresAt) {
    return snapshot;
  }
  return isSessionLive(snapshot.token, snapshot.sessionExpiresAt)
    ? snapshot
    : { ...snapshot, isAuthenticated: false };
}

/** Re-read localStorage into the snapshot (after clearAuthStorage / external writes). */
export function syncAuthSnapshotFromStorage(): AuthSnapshot {
  snapshot = readStorage();
  return snapshot;
}

/** Push known in-memory values (after applySecretToken / applySessionToken). */
export function setAuthSnapshot(token: string, sessionExpiresAt = ""): AuthSnapshot {
  snapshot = {
    token,
    sessionExpiresAt,
    isAuthenticated: isSessionLive(token, sessionExpiresAt),
  };
  return snapshot;
}
