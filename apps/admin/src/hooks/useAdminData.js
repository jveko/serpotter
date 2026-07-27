import { useCallback, useState } from "react";

import { PLAY_TOKEN_KEY } from "../constants.js";
import { adminFetch, apiBase } from "../api.js";

/**
 * Data-path state + refresh/mutations for the admin SPA.
 * Closed-over `secret` is used for mutations; `refresh(s)` always uses the
 * argument `s` for every request in the parallel bundle.
 * Optional `onAuthFail` runs on HTTP 401 (expired adm- session) so App can clearAuth.
 */
export function useAdminData(secret, { onAuthFail } = {}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [stats, setStats] = useState(null);
  const [tokens, setTokens] = useState([]);
  const [keys, setKeys] = useState([]);
  const [settings, setSettings] = useState(null);
  const [nodes, setNodes] = useState([]);
  const [requestLogs, setRequestLogs] = useState([]);
  const [newToken, setNewToken] = useState("");
  const [playToken, setPlayToken] = useState(
    () => localStorage.getItem(PLAY_TOKEN_KEY) || "",
  );
  const [playResult, setPlayResult] = useState(null);
  const [playErr, setPlayErr] = useState("");

  const reportError = useCallback(
    (e) => {
      setErr(e?.message || String(e));
      if (e?.status === 401 && typeof onAuthFail === "function") {
        onAuthFail();
      }
    },
    [onAuthFail],
  );

  const refresh = useCallback(
    async (s) => {
      setBusy(true);
      setErr("");
      try {
        const [st, tk, ky, set, nd, logs] = await Promise.all([
          adminFetch("/api/stats", s),
          adminFetch("/api/tokens", s),
          adminFetch("/api/keys", s),
          adminFetch("/api/settings", s),
          adminFetch("/api/nodes", s),
          adminFetch("/api/request-logs?limit=50", s),
        ]);
        setStats(st);
        setTokens(tk || []);
        setKeys(ky || []);
        setSettings(set);
        setNodes(nd || []);
        setRequestLogs(Array.isArray(logs) ? logs : []);
      } catch (e) {
        reportError(e);
        throw e;
      } finally {
        setBusy(false);
      }
    },
    [reportError],
  );

  const reset = useCallback(() => {
    setStats(null);
    setTokens([]);
    setKeys([]);
    setSettings(null);
    setNodes([]);
    setRequestLogs([]);
    setNewToken("");
    setPlayResult(null);
    setPlayErr("");
    setErr("");
    setBusy(false);
    // Intentionally leave playToken + PLAY_TOKEN_KEY intact (matches logout today).
  }, []);

  const createToken = useCallback(
    async ({ name }) => {
      setBusy(true);
      setErr("");
      try {
        const row = await adminFetch("/api/tokens", secret, {
          method: "POST",
          body: JSON.stringify({ name }),
        });
        setNewToken(row.token || "");
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const deleteToken = useCallback(
    async (id) => {
      if (!confirm(`Delete token #${id}?`)) return;
      setBusy(true);
      try {
        await adminFetch(`/api/tokens/${id}`, secret, { method: "DELETE" });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const createKey = useCallback(
    async ({ service, key }) => {
      setBusy(true);
      setErr("");
      try {
        await adminFetch("/api/keys", secret, {
          method: "POST",
          body: JSON.stringify({ service, key }),
        });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
        throw e2;
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const toggleKey = useCallback(
    async (id) => {
      setBusy(true);
      try {
        await adminFetch(`/api/keys/${id}/toggle`, secret, { method: "POST" });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const deleteKey = useCallback(
    async (id) => {
      if (!confirm(`Delete key #${id}?`)) return;
      setBusy(true);
      try {
        await adminFetch(`/api/keys/${id}`, secret, { method: "DELETE" });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const syncCredits = useCallback(
    async ({ service } = {}) => {
      setBusy(true);
      setErr("");
      try {
        const body = {};
        if (service) body.service = service;
        const report = await adminFetch("/api/keys/sync-credits", secret, {
          method: "POST",
          body: JSON.stringify(body),
        });
        const synced = Number(report?.synced ?? 0);
        const errors = Number(report?.errors ?? 0);
        const results = Array.isArray(report?.results) ? report.results : [];
        const failed = results.filter((r) => r && r.ok === false);
        const ok = results.filter((r) => r && r.ok === true);
        const failDetail =
          failed.length > 0
            ? `; failed: ${failed
                .map((r) =>
                  r.error ? `#${r.id}: ${r.error}` : `#${r.id}`,
                )
                .join(",")}`
            : "";
        const okDetail =
          ok.length > 0 && errors > 0
            ? `; ok: ${ok.map((r) => `#${r.id}`).join(",")}`
            : "";
        const partialMsg =
          errors > 0
            ? `Credit sync partial: synced=${synced}, errors=${errors}${failDetail}${okDetail} (exa/xai soft-fail or fetch error; keys stay active)`
            : "";
        // refresh clears err; re-apply partial message after lists reload
        await refresh(secret);
        if (partialMsg) setErr(partialMsg);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const saveSettings = useCallback(
    async ({ socialEnabled }) => {
      setBusy(true);
      setErr("");
      try {
        const out = await adminFetch("/api/settings", secret, {
          method: "PUT",
          body: JSON.stringify({ socialEnabled }),
        });
        setSettings(out);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, reportError],
  );

  const createNode = useCallback(
    async ({ host, port, username, password }) => {
      setBusy(true);
      setErr("");
      try {
        const body = {
          host: String(host ?? "").trim(),
          port: Number(port),
        };
        const user = username != null ? String(username).trim() : "";
        if (user) body.username = user;
        if (password) body.password = password;
        await adminFetch("/api/nodes", secret, {
          method: "POST",
          body: JSON.stringify(body),
        });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
        throw e2;
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const toggleNode = useCallback(
    async (id) => {
      setBusy(true);
      setErr("");
      try {
        await adminFetch(`/api/nodes/${id}/toggle`, secret, { method: "POST" });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const deleteNode = useCallback(
    async (id) => {
      if (!confirm(`Delete node #${id}?`)) return;
      setBusy(true);
      setErr("");
      try {
        await adminFetch(`/api/nodes/${id}`, secret, { method: "DELETE" });
        await refresh(secret);
      } catch (e2) {
        reportError(e2);
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh, reportError],
  );

  const refreshLogsOnly = useCallback(async () => {
    setBusy(true);
    setErr("");
    try {
      const logs = await adminFetch("/api/request-logs?limit=50", secret);
      setRequestLogs(Array.isArray(logs) ? logs : []);
    } catch (e2) {
      reportError(e2);
    } finally {
      setBusy(false);
    }
  }, [secret, reportError]);

  const runPlayground = useCallback(async ({ token, query, maxResults }) => {
    setPlayErr("");
    setPlayResult(null);
    setBusy(true);
    try {
      const res = await fetch(`${apiBase()}/api/search`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${String(token ?? "").trim()}`,
          "content-type": "application/json",
        },
        body: JSON.stringify({
          query: String(query ?? "").trim(),
          maxResults: Number(maxResults) || 5,
        }),
      });
      const text = await res.text();
      let data;
      try {
        data = text ? JSON.parse(text) : null;
      } catch {
        data = text;
      }
      if (!res.ok) {
        throw new Error(
          typeof data === "object" && data?.detail
            ? data.detail
            : typeof data === "object" && data?.title
              ? `${data.title}: ${data.detail || res.status}`
              : text || res.statusText,
        );
      }
      setPlayResult(data);
      localStorage.setItem(PLAY_TOKEN_KEY, String(token ?? "").trim());
    } catch (e2) {
      setPlayErr(String(e2.message || e2));
    } finally {
      setBusy(false);
    }
  }, []);

  const useInPlayground = useCallback((token) => {
    setPlayToken(token);
    localStorage.setItem(PLAY_TOKEN_KEY, token);
  }, []);

  return {
    busy,
    err,
    setErr,
    stats,
    tokens,
    keys,
    settings,
    setSettings,
    nodes,
    requestLogs,
    newToken,
    playToken,
    setPlayToken,
    playResult,
    playErr,
    refresh,
    reset,
    createToken,
    deleteToken,
    createKey,
    toggleKey,
    deleteKey,
    syncCredits,
    saveSettings,
    createNode,
    toggleNode,
    deleteNode,
    refreshLogsOnly,
    runPlayground,
    useInPlayground,
  };
}
