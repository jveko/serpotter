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

function readStorage(): AuthSnapshot {
  if (typeof localStorage === "undefined") {
    return { token: "", sessionExpiresAt: "", isAuthenticated: false };
  }
  const token = localStorage.getItem(SESSION_KEY) || localStorage.getItem(SECRET_KEY) || "";
  return {
    token,
    sessionExpiresAt: localStorage.getItem(SESSION_EXPIRES_KEY) || "",
    isAuthenticated: Boolean(token),
  };
}

let snapshot: AuthSnapshot = readStorage();

export function getAuthSnapshot(): AuthSnapshot {
  return snapshot;
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
    isAuthenticated: Boolean(token),
  };
  return snapshot;
}
