import { useCallback, useState } from "react";

import { PLAY_TOKEN_KEY } from "../constants.js";
import { adminFetch, apiBase } from "../api.js";

function playgroundHttpError(res, data, text) {
  if (typeof data === "object" && data !== null) {
    const title = data.title != null ? String(data.title).trim() : "";
    const detail = data.detail != null ? String(data.detail).trim() : "";
    if (title && detail) return `${res.status} ${title}: ${detail}`;
    if (title) return `${res.status} ${title}`;
    if (detail) return `${res.status} ${detail}`;
  }
  const fallback =
    (typeof data === "string" && data) ||
    text ||
    res.statusText ||
    "request failed";
  return `${res.status} ${fallback}`;
}

/**
 * Data-path state + refresh/mutations for the admin SPA.
 * Closed-over `secret` is used for mutations; `refresh(s)` always uses the
 * argument `s` for every request in the parallel bundle.
 * Optional `onAuthFail` runs on HTTP 401 (expired adm- session) so App can clearAuth.
 */
export function useAdminData(secret, { onAuthFail } = {}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");
  const [notice, setNotice] = useState("");
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
  const [playStatus, setPlayStatus] = useState(null);
  const [playErr, setPlayErr] = useState("");

  const reportError = useCallback(
    (e) => {
      setNotice("");
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
      setNotice("");
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
    setPlayStatus(null);
    setPlayErr("");
    setErr("");
    setNotice("");
    setBusy(false);
    // Intentionally leave playToken + PLAY_TOKEN_KEY intact (matches logout today).
  }, []);

  const createToken = useCallback(
    async ({ name }) => {
      setBusy(true);
      setErr("");
      setNotice("");
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
      setNotice("");
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
      setNotice("");
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
        await refresh(secret);
        if (errors > 0) {
          setNotice("");
          setErr(
            `Credit sync partial: synced=${synced}, errors=${errors}${failDetail}${okDetail} (exa/xai soft-fail or fetch error; keys stay active)`,
          );
        } else {
          setErr("");
          setNotice(`Credit sync: synced=${synced}, errors=0`);
        }
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
      setNotice("");
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
      setNotice("");
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
      setNotice("");
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
      setNotice("");
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
    setNotice("");
    try {
      const logs = await adminFetch("/api/request-logs?limit=50", secret);
      setRequestLogs(Array.isArray(logs) ? logs : []);
    } catch (e2) {
      reportError(e2);
    } finally {
      setBusy(false);
    }
  }, [secret, reportError]);

  const runPlayground = useCallback(
    async ({ token, mode = "search", query, maxResults, url, scrapeTopN }) => {
      setPlayErr("");
      setPlayResult(null);
      setPlayStatus(null);
      setBusy(true);
      try {
        const m = String(mode ?? "search").trim().toLowerCase() || "search";
        let path;
        let body;
        if (m === "extract") {
          path = "/api/extract";
          body = { url: String(url ?? "").trim() };
        } else if (m === "research") {
          path = "/api/research";
          body = { query: String(query ?? "").trim() };
          const maxN = Number(maxResults);
          if (Number.isFinite(maxN) && maxN > 0) {
            body.maxResults = maxN;
          }
          const scrapeN = Number(scrapeTopN);
          if (
            Number.isFinite(scrapeN) &&
            scrapeN >= 0 &&
            String(scrapeTopN ?? "").trim() !== ""
          ) {
            body.scrapeTopN = scrapeN;
          }
        } else {
          path = "/api/search";
          body = {
            query: String(query ?? "").trim(),
            maxResults: Number(maxResults) || 5,
          };
        }
        const res = await fetch(`${apiBase()}${path}`, {
          method: "POST",
          headers: {
            Authorization: `Bearer ${String(token ?? "").trim()}`,
            "content-type": "application/json",
          },
          body: JSON.stringify(body),
        });
        const text = await res.text();
        let data;
        try {
          data = text ? JSON.parse(text) : null;
        } catch {
          data = text;
        }
        setPlayStatus(res.status);

        if (!res.ok) {
          setPlayErr(playgroundHttpError(res, data, text));
          setPlayResult(null);
          return;
        }

        setPlayErr("");
        setPlayResult(data);
        localStorage.setItem(PLAY_TOKEN_KEY, String(token ?? "").trim());
      } catch (e2) {
        setPlayErr(String(e2.message || e2));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const useInPlayground = useCallback((token) => {
    setPlayToken(token);
    localStorage.setItem(PLAY_TOKEN_KEY, token);
  }, []);

  return {
    busy,
    err,
    notice,
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
    playStatus,
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
