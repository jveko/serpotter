import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  SECRET_KEY,
  SESSION_KEY,
  PLAY_TOKEN_KEY,
  SECTIONS,
} from "./constants.js";
import { apiBase, parseJsonResponse, adminFetch } from "./api.js";

export default function App() {
  const [secret, setSecret] = useState(
    () =>
      localStorage.getItem(SESSION_KEY) ||
      localStorage.getItem(SECRET_KEY) ||
      "",
  );
  const [loginMode, setLoginMode] = useState("secret"); // secret | password | bootstrap
  const [input, setInput] = useState(
    () => localStorage.getItem(SECRET_KEY) || "",
  );
  const [loginUser, setLoginUser] = useState("admin");
  const [loginPass, setLoginPass] = useState("");
  const [bootstrapSecret, setBootstrapSecret] = useState(
    () => localStorage.getItem(SECRET_KEY) || "",
  );
  const [err, setErr] = useState("");
  const [stats, setStats] = useState(null);
  const [tokens, setTokens] = useState([]);
  const [keys, setKeys] = useState([]);
  const [settings, setSettings] = useState(null);
  const [nodes, setNodes] = useState([]);
  const [requestLogs, setRequestLogs] = useState([]);
  const [nodeHost, setNodeHost] = useState("127.0.0.1");
  const [nodePort, setNodePort] = useState("7890");
  const [nodeUser, setNodeUser] = useState("");
  const [nodePass, setNodePass] = useState("");
  const [tokenName, setTokenName] = useState("admin");
  const [newToken, setNewToken] = useState("");
  const [keyService, setKeyService] = useState("tavily");
  const [keyValue, setKeyValue] = useState("");
  const [syncService, setSyncService] = useState(""); // "" = all
  const [busy, setBusy] = useState(false);
  const [playToken, setPlayToken] = useState(
    () => localStorage.getItem(PLAY_TOKEN_KEY) || "",
  );
  const [playQuery, setPlayQuery] = useState("rust axum");
  const [playMax, setPlayMax] = useState("5");
  const [playResult, setPlayResult] = useState(null);
  const [playErr, setPlayErr] = useState("");
  const [cmdkOpen, setCmdkOpen] = useState(false);
  const [cmdkQuery, setCmdkQuery] = useState("");
  const [cmdkIndex, setCmdkIndex] = useState(0);
  const cmdkInputRef = useRef(null);

  const loggedIn = useMemo(() => Boolean(secret), [secret]);

  const filteredSections = useMemo(() => {
    const q = cmdkQuery.trim().toLowerCase();
    if (!q) return SECTIONS;
    return SECTIONS.filter(
      (s) =>
        s.label.toLowerCase().includes(q) || s.id.toLowerCase().includes(q),
    );
  }, [cmdkQuery]);

  const jumpTo = useCallback((id) => {
    setCmdkOpen(false);
    setCmdkQuery("");
    setCmdkIndex(0);
    const el = document.getElementById(id);
    if (el) el.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  useEffect(() => {
    if (!loggedIn) return;
    function onKey(e) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdkOpen((open) => {
          if (open) {
            setCmdkQuery("");
            setCmdkIndex(0);
            return false;
          }
          setCmdkQuery("");
          setCmdkIndex(0);
          return true;
        });
        return;
      }
      if (!cmdkOpen) return;
      if (e.key === "Escape") {
        e.preventDefault();
        setCmdkOpen(false);
        setCmdkQuery("");
        setCmdkIndex(0);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [loggedIn, cmdkOpen]);

  useEffect(() => {
    if (cmdkOpen && cmdkInputRef.current) {
      cmdkInputRef.current.focus();
    }
  }, [cmdkOpen]);

  useEffect(() => {
    setCmdkIndex(0);
  }, [cmdkQuery]);

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

  useEffect(() => {
    if (secret) {
      refresh(secret).catch(() => {
        localStorage.removeItem(SECRET_KEY);
        localStorage.removeItem(SESSION_KEY);
        setSecret("");
      });
    }
  }, [secret, refresh]);

  function applySecretToken(s) {
    localStorage.setItem(SECRET_KEY, s);
    localStorage.removeItem(SESSION_KEY);
    setSecret(s);
  }

  async function loginWithSecret(e) {
    e.preventDefault();
    const s = input.trim();
    if (!s) return;
    try {
      // Temporarily use secret path (no session).
      localStorage.removeItem(SESSION_KEY);
      await refresh(s);
      applySecretToken(s);
    } catch {
      /* err already set */
    }
  }

  async function loginWithPassword(e) {
    e.preventDefault();
    const username = loginUser.trim() || "admin";
    const password = loginPass;
    if (!password) return;
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
      localStorage.setItem(SESSION_KEY, token);
      localStorage.removeItem(SECRET_KEY);
      await refresh(token);
      setSecret(token);
      setLoginPass("");
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  async function bootstrapThenLogin(e) {
    e.preventDefault();
    const adminSecret = bootstrapSecret.trim();
    const password = loginPass;
    const username = loginUser.trim() || "admin";
    if (!adminSecret || !password) return;
    setBusy(true);
    setErr("");
    try {
      const body = { password };
      if (loginUser.trim()) body.username = loginUser.trim();
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
      // Prefer session; keep secret out of storage after bootstrap path.
      localStorage.setItem(SESSION_KEY, token);
      localStorage.removeItem(SECRET_KEY);
      await refresh(token);
      setSecret(token);
      setLoginPass("");
      setBootstrapSecret("");
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  function logout() {
    const session = localStorage.getItem(SESSION_KEY);
    if (session) {
      // best-effort server logout
      fetch(`${apiBase()}/api/admin/logout`, {
        method: "POST",
        headers: { Authorization: `Bearer ${session}` },
      }).catch(() => {});
    }
    localStorage.removeItem(SECRET_KEY);
    localStorage.removeItem(SESSION_KEY);
    setSecret("");
    setStats(null);
    setTokens([]);
    setKeys([]);
    setSettings(null);
    setNewToken("");
    setNodes([]);
    setRequestLogs([]);
  }

  async function createToken(e) {
    e.preventDefault();
    setBusy(true);
    setErr("");
    try {
      const row = await adminFetch("/api/tokens", secret, {
        method: "POST",
        body: JSON.stringify({ name: tokenName }),
      });
      setNewToken(row.token || "");
      await refresh(secret);
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  async function deleteToken(id) {
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
  }

  async function createKey(e) {
    e.preventDefault();
    setBusy(true);
    setErr("");
    try {
      await adminFetch("/api/keys", secret, {
        method: "POST",
        body: JSON.stringify({ service: keyService, key: keyValue }),
      });
      setKeyValue("");
      await refresh(secret);
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  async function toggleKey(id) {
    setBusy(true);
    try {
      await adminFetch(`/api/keys/${id}/toggle`, secret, { method: "POST" });
      await refresh(secret);
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  async function deleteKey(id) {
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
  }

  async function syncCredits() {
    setBusy(true);
    setErr("");
    try {
      const body = {};
      if (syncService) body.service = syncService;
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
  }

  async function saveSettings(e) {
    e.preventDefault();
    if (!settings) return;
    setBusy(true);
    setErr("");
    try {
      const out = await adminFetch("/api/settings", secret, {
        method: "PUT",
        body: JSON.stringify({ socialEnabled: settings.socialEnabled }),
      });
      setSettings(out);
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  async function createNode(e) {
    e.preventDefault();
    setBusy(true);
    setErr("");
    try {
      const body = {
        host: nodeHost.trim(),
        port: Number(nodePort),
      };
      if (nodeUser.trim()) body.username = nodeUser.trim();
      if (nodePass) body.password = nodePass;
      await adminFetch("/api/nodes", secret, {
        method: "POST",
        body: JSON.stringify(body),
      });
      setNodePass("");
      await refresh(secret);
    } catch (e2) {
      setErr(e2.message || String(e2));
    } finally {
      setBusy(false);
    }
  }

  async function toggleNode(id) {
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
  }

  async function deleteNode(id) {
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
  }

  async function refreshLogsOnly() {
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
  }

  async function runPlayground(e) {
    e.preventDefault();
    setPlayErr("");
    setPlayResult(null);
    setBusy(true);
    try {
      const res = await fetch(`${apiBase()}/api/search`, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${playToken.trim()}`,
          "content-type": "application/json",
        },
        body: JSON.stringify({
          query: playQuery.trim(),
          maxResults: Number(playMax) || 5,
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
      localStorage.setItem(PLAY_TOKEN_KEY, playToken.trim());
    } catch (e2) {
      setPlayErr(String(e2.message || e2));
    } finally {
      setBusy(false);
    }
  }

  if (!loggedIn) {
    return (
      <div className="gate">
        <div className="gate__card">
          <div className="gate__brand">
            <span className="wordmark">
              Serpotter<span className="wordmark__dot">.</span>
            </span>
            <p className="kicker">Admin console</p>
          </div>
          <h1 className="gate__title">Sign in</h1>
          <p className="gate__lede">
            Authenticate with ADMIN_SECRET, password session, or first-time
            bootstrap.
          </p>

          <div className="gate__row">
            <button
              type="button"
              className={
                loginMode === "secret" ? "btn btn--primary btn--sm" : "btn btn--secondary btn--sm"
              }
              onClick={() => setLoginMode("secret")}
            >
              ADMIN_SECRET
            </button>
            <button
              type="button"
              className={
                loginMode === "password"
                  ? "btn btn--primary btn--sm"
                  : "btn btn--secondary btn--sm"
              }
              onClick={() => setLoginMode("password")}
            >
              Password
            </button>
            <button
              type="button"
              className={
                loginMode === "bootstrap"
                  ? "btn btn--primary btn--sm"
                  : "btn btn--secondary btn--sm"
              }
              onClick={() => setLoginMode("bootstrap")}
            >
              Bootstrap
            </button>
          </div>

          {loginMode === "secret" && (
            <form onSubmit={loginWithSecret} className="gate__form">
              <p className="field__hint">
                Enter <span className="mono">ADMIN_SECRET</span> (Bearer or
                X-Admin-Password).
              </p>
              <div className="row">
                <label className="field" style={{ flex: 1 }}>
                  <span className="field__label">Secret</span>
                  <input
                    className="input input--mono"
                    type="password"
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                    placeholder="ADMIN_SECRET"
                    autoComplete="current-password"
                  />
                </label>
                <button
                  type="submit"
                  className="btn btn--primary"
                  disabled={busy}
                  data-state={busy ? "loading" : undefined}
                >
                  Login
                </button>
              </div>
            </form>
          )}

          {loginMode === "password" && (
            <form onSubmit={loginWithPassword} className="gate__form">
              <p className="field__hint">
                Sign in with an admin user (after bootstrap). Stores{" "}
                <span className="mono">adm-</span> session token.
              </p>
              <div className="row">
                <label className="field">
                  <span className="field__label">Username</span>
                  <input
                    className="input input--narrow"
                    value={loginUser}
                    onChange={(e) => setLoginUser(e.target.value)}
                    placeholder="username"
                    autoComplete="username"
                  />
                </label>
                <label className="field" style={{ flex: 1 }}>
                  <span className="field__label">Password</span>
                  <input
                    className="input"
                    type="password"
                    value={loginPass}
                    onChange={(e) => setLoginPass(e.target.value)}
                    placeholder="password"
                    autoComplete="current-password"
                  />
                </label>
                <button
                  type="submit"
                  className="btn btn--primary"
                  disabled={busy || !loginPass}
                  data-state={busy ? "loading" : undefined}
                >
                  Login
                </button>
              </div>
            </form>
          )}

          {loginMode === "bootstrap" && (
            <form onSubmit={bootstrapThenLogin} className="gate__form">
              <p className="field__hint">
                First-time setup: create admin user with{" "}
                <span className="mono">ADMIN_SECRET</span>, then session login.
              </p>
              <div className="row">
                <label className="field" style={{ flex: 1 }}>
                  <span className="field__label">ADMIN_SECRET</span>
                  <input
                    className="input input--mono"
                    type="password"
                    value={bootstrapSecret}
                    onChange={(e) => setBootstrapSecret(e.target.value)}
                    placeholder="ADMIN_SECRET"
                  />
                </label>
                <label className="field">
                  <span className="field__label">Username</span>
                  <input
                    className="input input--narrow"
                    value={loginUser}
                    onChange={(e) => setLoginUser(e.target.value)}
                    placeholder="username (opt)"
                  />
                </label>
                <label className="field" style={{ flex: 1 }}>
                  <span className="field__label">Password</span>
                  <input
                    className="input"
                    type="password"
                    value={loginPass}
                    onChange={(e) => setLoginPass(e.target.value)}
                    placeholder="password"
                  />
                </label>
                <button
                  type="submit"
                  className="btn btn--primary"
                  disabled={busy || !bootstrapSecret.trim() || !loginPass}
                  data-state={busy ? "loading" : undefined}
                >
                  Bootstrap &amp; login
                </button>
              </div>
            </form>
          )}

          {err && <p className="err">{err}</p>}
        </div>
      </div>
    );
  }

  const byService = Array.isArray(stats?.byService) ? stats.byService : [];

  function onCmdkKeyDown(e) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setCmdkIndex((i) =>
        filteredSections.length
          ? Math.min(i + 1, filteredSections.length - 1)
          : 0,
      );
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setCmdkIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const sel = filteredSections[cmdkIndex];
      if (sel) jumpTo(sel.id);
    }
  }

  return (
    <div className="shell">
      <header className="topbar">
        <div className="topbar__inner">
          <span className="wordmark">
            Serpotter<span className="wordmark__dot">.</span>
          </span>
          <div className="topbar__meta">
            {stats?.schemaVersion != null && (
              <span className="chip chip--live">
                <span className="chip__swatch" aria-hidden />
                schema {stats.schemaVersion}
              </span>
            )}
            {busy && (
              <span className="chip chip--warn">
                <span className="chip__swatch" aria-hidden />
                busy
              </span>
            )}
          </div>
          <div className="topbar__actions">
            <button
              type="button"
              className="btn btn--kbd btn--sm"
              onClick={() => {
                setCmdkOpen(true);
                setCmdkQuery("");
                setCmdkIndex(0);
              }}
            >
              Jump <kbd>⌘K</kbd>
            </button>
            <button
              type="button"
              className="btn btn--secondary btn--sm"
              disabled={busy}
              data-state={busy ? "loading" : undefined}
              onClick={() => refresh(secret)}
            >
              Refresh
            </button>
            <button
              type="button"
              className="btn btn--ghost btn--sm"
              onClick={logout}
            >
              Logout
            </button>
          </div>
        </div>
      </header>

      <main className="shell__main">
        {err && (
          <div className="banner" role="alert">
            <p className="banner__text err">{err}</p>
          </div>
        )}

        <div className="workbench">
          <section className="panel" id="stats">
            <div className="panel__head">
              <h2 className="panel__title">Stats</h2>
              <span className="panel__meta">
                {stats ? "live" : "loading"}
              </span>
            </div>
            <div className="panel__body">
              {stats ? (
                <>
                  <div className="stat-strip">
                    <div className="stat">
                      <span className="stat__label">tokens</span>
                      <span className="stat__value">{stats.tokens}</span>
                    </div>
                    <div className="stat">
                      <span className="stat__label">keys</span>
                      <span className="stat__value">{stats.apiKeys}</span>
                    </div>
                    <div className="stat">
                      <span className="stat__label">active</span>
                      <span className="stat__value">{stats.activeApiKeys}</span>
                    </div>
                    <div className="stat">
                      <span className="stat__label">nodes</span>
                      <span className="stat__value">{stats.nodes}</span>
                    </div>
                    <div className="stat">
                      <span className="stat__label">schema</span>
                      <span className="stat__value">{stats.schemaVersion}</span>
                    </div>
                    <div className="stat">
                      <span className="stat__label">requestLogs</span>
                      <span className="stat__value">
                        {stats.requestLogs ?? 0}
                      </span>
                    </div>
                  </div>
                  {byService.length > 0 && (
                    <div className="table-wrap">
                      <table className="table">
                        <thead>
                          <tr>
                            <th>service</th>
                            <th>keys</th>
                            <th>active</th>
                            <th>creditsRemaining</th>
                            <th>creditsLimit</th>
                          </tr>
                        </thead>
                        <tbody>
                          {byService.map((s) => (
                            <tr key={s.service}>
                              <td>{s.service}</td>
                              <td>{s.keys}</td>
                              <td>{s.active}</td>
                              <td className="mono">
                                {s.creditsRemaining ?? "—"}
                              </td>
                              <td className="mono">
                                {s.creditsLimit ?? "—"}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>
                  )}
                </>
              ) : (
                <p className="empty">Loading…</p>
              )}
            </div>
          </section>

          <section className="panel" id="settings">
            <div className="panel__head">
              <h2 className="panel__title">Settings</h2>
            </div>
            <div className="panel__body">
              {settings ? (
                <form onSubmit={saveSettings} className="row">
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={Boolean(settings.socialEnabled)}
                      onChange={(e) =>
                        setSettings((prev) => ({
                          ...prev,
                          socialEnabled: e.target.checked,
                        }))
                      }
                    />
                    socialEnabled (research social leg)
                  </label>
                  <button
                    type="submit"
                    className="btn btn--primary btn--sm"
                    disabled={busy}
                    data-state={busy ? "loading" : undefined}
                  >
                    Save settings
                  </button>
                </form>
              ) : (
                <p className="empty">Loading…</p>
              )}
            </div>
          </section>

          <section className="panel" id="tokens">
            <div className="panel__head">
              <h2 className="panel__title">API tokens</h2>
            </div>
            <div className="panel__body">
              <form onSubmit={createToken} className="row">
                <label className="field">
                  <span className="field__label">Name</span>
                  <input
                    className="input"
                    value={tokenName}
                    onChange={(e) => setTokenName(e.target.value)}
                    placeholder="name"
                  />
                </label>
                <button
                  type="submit"
                  className="btn btn--primary btn--sm"
                  disabled={busy}
                  data-state={busy ? "loading" : undefined}
                >
                  Create token
                </button>
              </form>
              {newToken && (
                <>
                  <p className="mono break">
                    New token (copy once): {newToken}
                  </p>
                  <button
                    type="button"
                    className="btn btn--secondary btn--sm"
                    onClick={() => {
                      setPlayToken(newToken);
                      localStorage.setItem(PLAY_TOKEN_KEY, newToken);
                    }}
                  >
                    Use in playground
                  </button>
                </>
              )}
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>id</th>
                      <th>name</th>
                      <th>preview</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {tokens.map((t) => (
                      <tr key={t.id}>
                        <td>{t.id}</td>
                        <td>{t.name}</td>
                        <td className="mono">{t.tokenPreview}</td>
                        <td className="table__actions">
                          <button
                            type="button"
                            className="btn btn--secondary btn--sm"
                            onClick={() => deleteToken(t.id)}
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </section>

          <section className="panel" id="keys">
            <div className="panel__head">
              <h2 className="panel__title">Provider keys</h2>
            </div>
            <div className="panel__body">
              <form onSubmit={createKey} className="row">
                <label className="field">
                  <span className="field__label">Service</span>
                  <select
                    className="select"
                    value={keyService}
                    onChange={(e) => setKeyService(e.target.value)}
                  >
                    <option value="tavily">tavily</option>
                    <option value="firecrawl">firecrawl</option>
                    <option value="exa">exa</option>
                    <option value="xai">xai</option>
                  </select>
                </label>
                <label className="field" style={{ flex: 1 }}>
                  <span className="field__label">API key</span>
                  <input
                    className="input input--mono"
                    value={keyValue}
                    onChange={(e) => setKeyValue(e.target.value)}
                    placeholder="api key"
                  />
                </label>
                <button
                  type="submit"
                  className="btn btn--primary btn--sm"
                  disabled={busy || !keyValue}
                  data-state={busy ? "loading" : undefined}
                >
                  Seed key
                </button>
              </form>
              <div className="row row--tight">
                <label className="field">
                  <span className="field__label">Sync service</span>
                  <select
                    className="select"
                    value={syncService}
                    onChange={(e) => setSyncService(e.target.value)}
                  >
                    <option value="">all (tavily+firecrawl)</option>
                    <option value="tavily">tavily</option>
                    <option value="firecrawl">firecrawl</option>
                  </select>
                </label>
                <button
                  type="button"
                  className="btn btn--secondary btn--sm"
                  disabled={busy}
                  data-state={busy ? "loading" : undefined}
                  onClick={syncCredits}
                >
                  Sync credits
                </button>
              </div>
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>id</th>
                      <th>service</th>
                      <th>preview</th>
                      <th>active</th>
                      <th>fails</th>
                      <th>creditsRemaining</th>
                      <th>creditsLimit</th>
                      <th>usageSyncedAt</th>
                      <th>inflight</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {keys.map((k) => (
                      <tr key={k.id}>
                        <td>{k.id}</td>
                        <td>{k.service}</td>
                        <td className="mono">{k.keyPreview}</td>
                        <td>{k.active ? "yes" : "no"}</td>
                        <td>{k.consecutiveFails}</td>
                        <td className="mono">{k.creditsRemaining ?? "—"}</td>
                        <td className="mono">{k.creditsLimit ?? "—"}</td>
                        <td className="mono">{k.usageSyncedAt || "—"}</td>
                        <td>{k.inflight ?? 0}</td>
                        <td className="table__actions">
                          <button
                            type="button"
                            className="btn btn--secondary btn--sm"
                            onClick={() => toggleKey(k.id)}
                          >
                            Toggle
                          </button>
                          <button
                            type="button"
                            className="btn btn--danger btn--sm"
                            onClick={() => deleteKey(k.id)}
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </section>

          <section className="panel" id="nodes">
            <div className="panel__head">
              <h2 className="panel__title">Outbound nodes</h2>
            </div>
            <div className="panel__body">
              <p className="panel__lede">
                Optional HTTP proxies for Tavily/Firecrawl/Exa (xAI always
                direct). Boot resolves OUTBOUND_PROXY env first, else enabled
                node URL.
              </p>
              <form onSubmit={createNode} className="row">
                <label className="field">
                  <span className="field__label">Host</span>
                  <input
                    className="input"
                    value={nodeHost}
                    onChange={(e) => setNodeHost(e.target.value)}
                    placeholder="host"
                    required
                  />
                </label>
                <label className="field">
                  <span className="field__label">Port</span>
                  <input
                    className="input input--port"
                    value={nodePort}
                    onChange={(e) => setNodePort(e.target.value)}
                    placeholder="port"
                    required
                  />
                </label>
                <label className="field">
                  <span className="field__label">Username</span>
                  <input
                    className="input"
                    value={nodeUser}
                    onChange={(e) => setNodeUser(e.target.value)}
                    placeholder="username (opt)"
                  />
                </label>
                <label className="field">
                  <span className="field__label">Password</span>
                  <input
                    className="input"
                    type="password"
                    value={nodePass}
                    onChange={(e) => setNodePass(e.target.value)}
                    placeholder="password (opt)"
                  />
                </label>
                <button
                  type="submit"
                  className="btn btn--primary btn--sm"
                  disabled={busy || !nodeHost.trim()}
                  data-state={busy ? "loading" : undefined}
                >
                  Add node
                </button>
              </form>
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>id</th>
                      <th>host</th>
                      <th>port</th>
                      <th>user</th>
                      <th>enabled</th>
                      <th>inflight</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {nodes.map((n) => (
                      <tr key={n.id}>
                        <td>{n.id}</td>
                        <td className="mono">{n.host}</td>
                        <td>{n.port}</td>
                        <td className="mono">{n.username || "—"}</td>
                        <td>{n.enabled ? "yes" : "no"}</td>
                        <td>{n.inflight}</td>
                        <td className="table__actions">
                          <button
                            type="button"
                            className="btn btn--secondary btn--sm"
                            onClick={() => toggleNode(n.id)}
                          >
                            {n.enabled ? "Disable" : "Enable"}
                          </button>
                          <button
                            type="button"
                            className="btn btn--danger btn--sm"
                            onClick={() => deleteNode(n.id)}
                          >
                            Delete
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          </section>

          <section className="panel" id="logs">
            <div className="panel__head">
              <h2 className="panel__title">Request logs</h2>
              <div className="panel__meta">
                <button
                  type="button"
                  className="btn btn--secondary btn--sm"
                  disabled={busy}
                  data-state={busy ? "loading" : undefined}
                  onClick={refreshLogsOnly}
                >
                  Refresh logs
                </button>
              </div>
            </div>
            <div className="panel__body">
              <p className="panel__lede">
                Latest 50 from GET /api/request-logs (newest first).
              </p>
              <div className="table-wrap">
                <table className="table">
                  <thead>
                    <tr>
                      <th>id</th>
                      <th>createdAt</th>
                      <th>path</th>
                      <th>status</th>
                      <th>service</th>
                      <th>providerUsed</th>
                      <th>durationMs</th>
                      <th>errorKind</th>
                      <th>queryPreview</th>
                    </tr>
                  </thead>
                  <tbody>
                    {requestLogs.length === 0 ? (
                      <tr>
                        <td colSpan={9} className="empty">
                          No logs
                        </td>
                      </tr>
                    ) : (
                      requestLogs.map((r) => (
                        <tr key={r.id}>
                          <td>{r.id}</td>
                          <td className="mono">{r.createdAt}</td>
                          <td className="mono">{r.path}</td>
                          <td>{r.status}</td>
                          <td>{r.service || "—"}</td>
                          <td>{r.providerUsed || "—"}</td>
                          <td>{r.durationMs ?? "—"}</td>
                          <td className="mono">{r.errorKind || "—"}</td>
                          <td className="mono break">
                            {r.queryPreview || "—"}
                          </td>
                        </tr>
                      ))
                    )}
                  </tbody>
                </table>
              </div>
            </div>
          </section>

          <section className="panel panel--graphite" id="playground">
            <div className="panel__head">
              <h2 className="panel__title">Search playground</h2>
            </div>
            <div className="panel__body">
              <p className="panel__lede">
                Calls POST /api/search with a client token (tok-…), not
                ADMIN_SECRET.
              </p>
              <form onSubmit={runPlayground}>
                <div className="row">
                  <label className="field" style={{ flex: 1 }}>
                    <span className="field__label">API token</span>
                    <input
                      className="input input--mono"
                      value={playToken}
                      onChange={(e) => setPlayToken(e.target.value)}
                      placeholder="tok-… API token"
                      required
                    />
                  </label>
                </div>
                <div className="row">
                  <label className="field" style={{ flex: 1 }}>
                    <span className="field__label">Query</span>
                    <input
                      className="input"
                      value={playQuery}
                      onChange={(e) => setPlayQuery(e.target.value)}
                      placeholder="query"
                      required
                    />
                  </label>
                  <label className="field">
                    <span className="field__label">Max</span>
                    <input
                      className="input input--narrow"
                      value={playMax}
                      onChange={(e) => setPlayMax(e.target.value)}
                      placeholder="max"
                    />
                  </label>
                  <button
                    type="submit"
                    className="btn btn--primary btn--sm"
                    disabled={busy || !playToken.trim() || !playQuery.trim()}
                    data-state={busy ? "loading" : undefined}
                  >
                    Search
                  </button>
                </div>
              </form>
              {playErr && <p className="err">{playErr}</p>}
              {playResult && (
                <div>
                  <div className="pre__label">
                    <span>response</span>
                    <span className="chip chip--ok">200 OK</span>
                  </div>
                  <pre className="pre mono">
                    {JSON.stringify(playResult, null, 2)}
                  </pre>
                </div>
              )}
            </div>
          </section>
        </div>

        <footer className="colophon">
          <p>Serpotter admin · Cobalt instrument panel</p>
        </footer>
      </main>

      {cmdkOpen && (
        <div
          className="cmdk-backdrop"
          role="presentation"
          onClick={() => {
            setCmdkOpen(false);
            setCmdkQuery("");
            setCmdkIndex(0);
          }}
        >
          <div
            className="cmdk"
            role="dialog"
            aria-label="Jump to section"
            onClick={(e) => e.stopPropagation()}
          >
            <input
              ref={cmdkInputRef}
              className="cmdk__input"
              value={cmdkQuery}
              onChange={(e) => setCmdkQuery(e.target.value)}
              onKeyDown={onCmdkKeyDown}
              placeholder="Jump to section…"
              aria-autocomplete="list"
            />
            <ul className="cmdk__list" role="listbox">
              {filteredSections.length === 0 ? (
                <li className="cmdk__empty">No matches</li>
              ) : (
                filteredSections.map((s, i) => (
                  <li key={s.id}>
                    <button
                      type="button"
                      className={
                        i === cmdkIndex
                          ? "cmdk__item is-active"
                          : "cmdk__item"
                      }
                      aria-selected={i === cmdkIndex}
                      onMouseEnter={() => setCmdkIndex(i)}
                      onClick={() => jumpTo(s.id)}
                    >
                      {s.label}
                      <span className="cmdk__hint">#{s.id}</span>
                    </button>
                  </li>
                ))
              )}
            </ul>
          </div>
        </div>
      )}
    </div>
  );
}
