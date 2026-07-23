import { useCallback, useState } from "react";

import { PLAY_TOKEN_KEY } from "../constants.js";
import { adminFetch, apiBase } from "../api.js";

/**
 * Data-path state + refresh/mutations for the admin SPA.
 * Closed-over `secret` is used for mutations; `refresh(s)` always uses the
 * argument `s` for every request in the parallel bundle.
 */
export function useAdminData(secret) {
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

  const refresh = useCallback(async (s) => {
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
      setErr(e.message || String(e));
      throw e;
    } finally {
      setBusy(false);
    }
  }, []);

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
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const deleteToken = useCallback(
    async (id) => {
      if (!confirm(`Delete token #${id}?`)) return;
      setBusy(true);
      try {
        await adminFetch(`/api/tokens/${id}`, secret, { method: "DELETE" });
        await refresh(secret);
      } catch (e2) {
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
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
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const toggleKey = useCallback(
    async (id) => {
      setBusy(true);
      try {
        await adminFetch(`/api/keys/${id}/toggle`, secret, { method: "POST" });
        await refresh(secret);
      } catch (e2) {
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const deleteKey = useCallback(
    async (id) => {
      if (!confirm(`Delete key #${id}?`)) return;
      setBusy(true);
      try {
        await adminFetch(`/api/keys/${id}`, secret, { method: "DELETE" });
        await refresh(secret);
      } catch (e2) {
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const syncCredits = useCallback(
    async ({ service } = {}) => {
      setBusy(true);
      setErr("");
      try {
        const body = {};
        if (service) body.service = service;
        await adminFetch("/api/keys/sync-credits", secret, {
          method: "POST",
          body: JSON.stringify(body),
        });
        await refresh(secret);
      } catch (e2) {
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
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
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret],
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
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const toggleNode = useCallback(
    async (id) => {
      setBusy(true);
      setErr("");
      try {
        await adminFetch(`/api/nodes/${id}/toggle`, secret, { method: "POST" });
        await refresh(secret);
      } catch (e2) {
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const deleteNode = useCallback(
    async (id) => {
      setBusy(true);
      setErr("");
      try {
        await adminFetch(`/api/nodes/${id}`, secret, { method: "DELETE" });
        await refresh(secret);
      } catch (e2) {
        setErr(e2.message || String(e2));
      } finally {
        setBusy(false);
      }
    },
    [secret, refresh],
  );

  const refreshLogsOnly = useCallback(async () => {
    setBusy(true);
    setErr("");
    try {
      const logs = await adminFetch("/api/request-logs?limit=50", secret);
      setRequestLogs(Array.isArray(logs) ? logs : []);
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }, [secret]);

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
