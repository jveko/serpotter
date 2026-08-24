import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

import { apiBase, parseJsonResponse } from "@/lib/api";
import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

import { clearAuthStorage } from "./session-end";
import {
  onAuthStorageChanged,
  parseSessionExpiry,
  setAuthSnapshot,
  syncAuthSnapshotFromStorage,
} from "./auth-snapshot";
import type { AuthContextValue } from "./types";

type LoginBody = {
  token?: string;
  expiresAt?: string;
  expires_at?: string;
};

const AuthContext = createContext<AuthContextValue | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [token, setToken] = useState(() => syncAuthSnapshotFromStorage().token);
  const [sessionExpiresAt, setSessionExpiresAt] = useState(
    () => syncAuthSnapshotFromStorage().sessionExpiresAt,
  );
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  // Sync React state when endAdminSession clears storage + dispatches, and when
  // another tab logs in/out (storage event → cross-tab snapshot re-read).
  useEffect(() => {
    const fn = () => {
      setToken("");
      setSessionExpiresAt("");
      setErr("");
      setBusy(false);
    };
    window.addEventListener("serpotter:auth-cleared", fn);
    const unsubscribe = onAuthStorageChanged((next) => {
      setToken(next.token);
      setSessionExpiresAt(next.sessionExpiresAt);
      setErr("");
      setBusy(false);
    });
    return () => {
      window.removeEventListener("serpotter:auth-cleared", fn);
      unsubscribe();
    };
  }, []);

  const isAuthenticated = useMemo(
    () =>
      Boolean(token) && (!sessionExpiresAt || parseSessionExpiry(sessionExpiresAt) > Date.now()),
    [token, sessionExpiresAt],
  );

  const applySecretToken = useCallback((s: string) => {
    localStorage.setItem(SECRET_KEY, s);
    localStorage.removeItem(SESSION_KEY);
    localStorage.removeItem(SESSION_EXPIRES_KEY);
    setAuthSnapshot(s, "");
    setToken(s);
    setSessionExpiresAt("");
    setErr("");
  }, []);

  const applySessionToken = useCallback((t: string, expiresAt?: string) => {
    localStorage.setItem(SESSION_KEY, t);
    localStorage.removeItem(SECRET_KEY);
    if (expiresAt) {
      localStorage.setItem(SESSION_EXPIRES_KEY, String(expiresAt));
      setAuthSnapshot(t, String(expiresAt));
      setSessionExpiresAt(String(expiresAt));
    } else {
      localStorage.removeItem(SESSION_EXPIRES_KEY);
      setAuthSnapshot(t, "");
      setSessionExpiresAt("");
    }
    setToken(t);
    setErr("");
  }, []);

  const clearAuth = useCallback(() => {
    clearAuthStorage();
    setToken("");
    setSessionExpiresAt("");
    setErr("");
    setBusy(false);
  }, []);

  // Session-lapse teardown: once the stored window passes, clear auth so the
  // context, storage, and the route-guard snapshot all agree (an idle tab past
  // expiry still lands on login). Heartbeat keeps the check live without
  // storage writes on every render.
  useEffect(() => {
    if (!sessionExpiresAt) return;
    const id = window.setInterval(() => {
      if (parseSessionExpiry(sessionExpiresAt) <= Date.now()) {
        clearAuth();
      }
    }, 30_000);
    return () => window.clearInterval(id);
  }, [sessionExpiresAt, clearAuth]);

  // Immediate teardown when a render observes an already-lapsed window
  // (e.g. state changed after expiry but before the next heartbeat).
  useEffect(() => {
    if (token && !isAuthenticated) {
      clearAuth();
    }
  }, [token, isAuthenticated, clearAuth]);

  const logout = useCallback(() => {
    const session = localStorage.getItem(SESSION_KEY);
    if (session) {
      void fetch(`${apiBase()}/api/admin/logout`, {
        method: "POST",
        headers: { Authorization: `Bearer ${session}` },
      }).catch(() => {});
    }
    clearAuth();
  }, [clearAuth]);

  /**
   * POST /api/admin/login. Sets session busy/err. Returns { token, expiresAt }; does not apply storage.
   */
  const loginWithPasswordHttp = useCallback(
    async ({ username, password }: { username: string; password: string }) => {
      setBusy(true);
      setErr("");
      try {
        const res = await fetch(`${apiBase()}/api/admin/login`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ username, password }),
        });
        const data = await parseJsonResponse<LoginBody>(res);
        const nextToken = data?.token;
        if (!nextToken) throw new Error("login response missing token");
        return {
          token: nextToken,
          expiresAt: data?.expiresAt || data?.expires_at || "",
        };
      } catch (e2) {
        const message = e2 instanceof Error ? e2.message : String(e2);
        setErr(message);
        throw e2;
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  /**
   * Bootstrap admin user then login. Sets session busy/err. Returns { token, expiresAt }.
   */
  const bootstrapHttp = useCallback(
    async ({
      adminSecret,
      loginUser,
      password,
    }: {
      adminSecret: string;
      loginUser: string;
      password: string;
    }) => {
      setBusy(true);
      setErr("");
      try {
        const username = (loginUser || "").trim() || "admin";
        const body: { password: string; username?: string } = { password };
        // Match App: only include username on bootstrap body when non-empty trim
        if ((loginUser || "").trim()) body.username = (loginUser || "").trim();
        const bootRes = await fetch(`${apiBase()}/api/admin/bootstrap`, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            Authorization: `Bearer ${adminSecret}`,
          },
          body: JSON.stringify(body),
        });
        await parseJsonResponse(bootRes);

        const loginRes = await fetch(`${apiBase()}/api/admin/login`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ username, password }),
        });
        const data = await parseJsonResponse<LoginBody>(loginRes);
        const nextToken = data?.token;
        if (!nextToken) throw new Error("login response missing token");
        return {
          token: nextToken,
          expiresAt: data?.expiresAt || data?.expires_at || "",
        };
      } catch (e2) {
        const message = e2 instanceof Error ? e2.message : String(e2);
        setErr(message);
        throw e2;
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const value = useMemo<AuthContextValue>(
    () => ({
      token,
      sessionExpiresAt,
      isAuthenticated,
      busy,
      err,
      setErr,
      applySecretToken,
      applySessionToken,
      clearAuth,
      logout,
      loginWithPasswordHttp,
      bootstrapHttp,
    }),
    [
      token,
      sessionExpiresAt,
      isAuthenticated,
      busy,
      err,
      applySecretToken,
      applySessionToken,
      clearAuth,
      logout,
      loginWithPasswordHttp,
      bootstrapHttp,
    ],
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) {
    throw new Error("useAuth must be used within AuthProvider");
  }
  return ctx;
}
