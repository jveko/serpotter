import { useCallback, useMemo, useState } from "react";

import { apiBase, parseJsonResponse } from "../api.js";
import { SECRET_KEY, SESSION_KEY } from "../constants.js";

/**
 * Session / auth state for the admin SPA.
 * HTTP helpers return a token string; callers apply storage + data.refresh.
 * Does not import or call useAdminData / refresh.
 */
export function useAdminSession() {
  const [secret, setSecret] = useState(
    () =>
      localStorage.getItem(SESSION_KEY) ||
      localStorage.getItem(SECRET_KEY) ||
      "",
  );
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const loggedIn = useMemo(() => Boolean(secret), [secret]);

  const applySecretToken = useCallback((s) => {
    localStorage.setItem(SECRET_KEY, s);
    localStorage.removeItem(SESSION_KEY);
    setSecret(s);
    setErr("");
  }, []);

  const applySessionToken = useCallback((token) => {
    localStorage.setItem(SESSION_KEY, token);
    localStorage.removeItem(SECRET_KEY);
    setSecret(token);
    setErr("");
  }, []);

  const clearAuth = useCallback(() => {
    localStorage.removeItem(SECRET_KEY);
    localStorage.removeItem(SESSION_KEY);
    setSecret("");
    setErr("");
    setBusy(false);
  }, []);

  const logout = useCallback(() => {
    const session = localStorage.getItem(SESSION_KEY);
    if (session) {
      // best-effort server logout
      fetch(`${apiBase()}/api/admin/logout`, {
        method: "POST",
        headers: { Authorization: `Bearer ${session}` },
      }).catch(() => {});
    }
    clearAuth();
  }, [clearAuth]);

  /**
   * POST /api/admin/login. Sets session busy/err. Returns token; does not apply storage.
   */
  const loginWithPasswordHttp = useCallback(async ({ username, password }) => {
    setBusy(true);
    setErr("");
    try {
      const res = await fetch(`${apiBase()}/api/admin/login`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ username, password }),
      });
      const data = await parseJsonResponse(res);
      const token = data?.token;
      if (!token) throw new Error("login response missing token");
      return token;
    } catch (e2) {
      setErr(e2.message || String(e2));
      throw e2;
    } finally {
      setBusy(false);
    }
  }, []);

  /**
   * Bootstrap admin user then login. Sets session busy/err. Returns token; does not apply storage.
   */
  const bootstrapHttp = useCallback(
    async ({ adminSecret, loginUser, password }) => {
      setBusy(true);
      setErr("");
      try {
        const username = (loginUser || "").trim() || "admin";
        const body = { password };
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
        const data = await parseJsonResponse(loginRes);
        const token = data?.token;
        if (!token) throw new Error("login response missing token");
        return token;
      } catch (e2) {
        setErr(e2.message || String(e2));
        throw e2;
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  return {
    secret,
    loggedIn,
    busy,
    err,
    setErr,
    applySecretToken,
    applySessionToken,
    clearAuth,
    logout,
    loginWithPasswordHttp,
    bootstrapHttp,
  };
}
