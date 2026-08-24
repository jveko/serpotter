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

/** Admin-identity keys that drive cross-tab auth sync. */
const AUTH_STORAGE_KEYS = [SECRET_KEY, SESSION_KEY, SESSION_EXPIRES_KEY] as const;

/**
 * Parse the admin session expiry as UTC epoch ms. The backend writes
 * `expires_at` via SQLite `datetime('now', '+7 days')` — a space-separated UTC
 * stamp with no zone designator ("YYYY-MM-DD HH:MM:SS"). `new Date(...)` reads
 * that shape as LOCAL time, which would log a UTC+7 operator out ~7h early.
 * ISO-8601 values pass through unchanged; zone-less ISO is treated as UTC per
 * the backend contract. Returns 0 for empty/unparseable values (never NaN).
 */
export function parseSessionExpiry(value: string | null | undefined): number {
  if (!value) return 0;
  const withT = value.includes("T") ? value : value.replace(" ", "T");
  const iso = /(?:Z|[+-]\d{2}:\d{2})$/.test(withT) ? withT : `${withT}Z`;
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : 0;
}

/** True only when a token exists AND the session window (if any) has not lapsed. */
function isSessionLive(token: string, sessionExpiresAt: string): boolean {
  return Boolean(token) && (!sessionExpiresAt || parseSessionExpiry(sessionExpiresAt) > Date.now());
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

/**
 * Cross-tab auth sync. localStorage changes fire a `storage` event only in
 * OTHER tabs of the same origin — the originating tab gets nothing. Tab A's
 * logout (clearAuthStorage removes SESSION_KEY/SECRET_KEY/SESSION_EXPIRES_KEY)
 * therefore arrives here as removal events in every other tab. The handler
 * receives the freshly re-read snapshot so the React side can reset auth
 * state; route guards re-check via getAuthSnapshot().
 */
export function onAuthStorageChanged(handler: (snapshot: AuthSnapshot) => void): () => void {
  if (typeof window === "undefined" || typeof window.addEventListener !== "function") {
    return () => {};
  }
  const fn = (e: StorageEvent) => {
    if (e.storageArea !== localStorage) return;
    if (e.key !== null && !(AUTH_STORAGE_KEYS as readonly string[]).includes(e.key)) return;
    // Update the module snapshot too: route guards call getAuthSnapshot()
    // (not the React callback), so a cross-tab logout must not leave them
    // reading the stale authenticated claim.
    snapshot = readStorage();
    handler(snapshot);
  };
  window.addEventListener("storage", fn);
  return () => window.removeEventListener("storage", fn);
}
