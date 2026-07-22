import React, { useCallback, useEffect, useMemo, useState } from "react";

const SECRET_KEY = "serpotter_admin_secret";

function apiBase() {
  return import.meta.env.VITE_API_BASE || "";
}

async function adminFetch(path, secret, opts = {}) {
  const headers = {
    ...(opts.headers || {}),
    Authorization: `Bearer ${secret}`,
  };
  if (opts.body && !headers["content-type"]) {
    headers["content-type"] = "application/json";
  }
  const res = await fetch(`${apiBase()}${path}`, { ...opts, headers });
  const text = await res.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (!res.ok) {
    const msg = data?.detail || data?.title || res.statusText || "request failed";
    throw new Error(msg);
  }
  return data;
}

export default function App() {
  const [secret, setSecret] = useState(() => localStorage.getItem(SECRET_KEY) || "");
  const [input, setInput] = useState(secret);
  const [err, setErr] = useState("");
  const [stats, setStats] = useState(null);
  const [tokens, setTokens] = useState([]);
  const [keys, setKeys] = useState([]);
  const [tokenName, setTokenName] = useState("admin");
  const [newToken, setNewToken] = useState("");
  const [keyService, setKeyService] = useState("tavily");
  const [keyValue, setKeyValue] = useState("");
  const [busy, setBusy] = useState(false);

  const loggedIn = useMemo(() => Boolean(secret), [secret]);

  const refresh = useCallback(async (s) => {
    setBusy(true);
    setErr("");
    try {
      const [st, tk, ky] = await Promise.all([
        adminFetch("/api/stats", s),
        adminFetch("/api/tokens", s),
        adminFetch("/api/keys", s),
      ]);
      setStats(st);
      setTokens(tk || []);
      setKeys(ky || []);
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
        setSecret("");
      });
    }
  }, [secret, refresh]);

  async function login(e) {
    e.preventDefault();
    const s = input.trim();
    if (!s) return;
    try {
      await refresh(s);
      localStorage.setItem(SECRET_KEY, s);
      setSecret(s);
    } catch {
      /* err already set */
    }
  }

  function logout() {
    localStorage.removeItem(SECRET_KEY);
    setSecret("");
    setStats(null);
    setTokens([]);
    setKeys([]);
    setNewToken("");
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

  if (!loggedIn) {
    return (
      <div className="wrap">
        <h1>Serpotter Admin</h1>
        <div className="card">
          <p className="muted">
            Enter <span className="mono">ADMIN_SECRET</span> (Bearer or X-Admin-Password).
          </p>
          <form onSubmit={login} className="row">
            <input
              type="password"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              placeholder="ADMIN_SECRET"
              style={{ flex: 1, minWidth: 200 }}
            />
            <button type="submit" disabled={busy}>
              Login
            </button>
          </form>
          {err && <p className="err">{err}</p>}
        </div>
      </div>
    );
  }

  return (
    <div className="wrap">
      <div className="row" style={{ justifyContent: "space-between" }}>
        <h1>Serpotter Admin</h1>
        <button type="button" className="secondary" onClick={logout}>
          Logout
        </button>
      </div>
      {err && <p className="err">{err}</p>}

      <div className="card">
        <h2 style={{ marginTop: 0, fontSize: "1rem" }}>Stats</h2>
        {stats ? (
          <div className="row muted">
            <span>tokens: {stats.tokens}</span>
            <span>keys: {stats.apiKeys}</span>
            <span>active: {stats.activeApiKeys}</span>
            <span>nodes: {stats.nodes}</span>
            <span>schema: {stats.schemaVersion}</span>
          </div>
        ) : (
          <p className="muted">Loading…</p>
        )}
        <button type="button" className="secondary" disabled={busy} onClick={() => refresh(secret)}>
          Refresh
        </button>
      </div>

      <div className="card">
        <h2 style={{ marginTop: 0, fontSize: "1rem" }}>API tokens</h2>
        <form onSubmit={createToken} className="row">
          <input
            value={tokenName}
            onChange={(e) => setTokenName(e.target.value)}
            placeholder="name"
          />
          <button type="submit" disabled={busy}>
            Create token
          </button>
        </form>
        {newToken && (
          <p className="mono" style={{ wordBreak: "break-all" }}>
            New token (copy once): {newToken}
          </p>
        )}
        <table>
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
                <td>
                  <button type="button" className="secondary" onClick={() => deleteToken(t.id)}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="card">
        <h2 style={{ marginTop: 0, fontSize: "1rem" }}>Provider keys</h2>
        <form onSubmit={createKey} className="row">
          <select value={keyService} onChange={(e) => setKeyService(e.target.value)}>
            <option value="tavily">tavily</option>
            <option value="firecrawl">firecrawl</option>
            <option value="exa">exa</option>
            <option value="xai">xai</option>
          </select>
          <input
            value={keyValue}
            onChange={(e) => setKeyValue(e.target.value)}
            placeholder="api key"
            style={{ flex: 1, minWidth: 180 }}
          />
          <button type="submit" disabled={busy || !keyValue}>
            Seed key
          </button>
        </form>
        <table>
          <thead>
            <tr>
              <th>id</th>
              <th>service</th>
              <th>preview</th>
              <th>active</th>
              <th>fails</th>
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
                <td className="row">
                  <button type="button" className="secondary" onClick={() => toggleKey(k.id)}>
                    Toggle
                  </button>
                  <button type="button" className="secondary" onClick={() => deleteKey(k.id)}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
