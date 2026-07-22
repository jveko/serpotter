import React, { useCallback, useEffect, useMemo, useState } from "react";

const SECRET_KEY = "serpotter_admin_secret";
const SESSION_KEY = "serpotter_admin_session";
const PLAY_TOKEN_KEY = "serpotter_play_token";

function apiBase() {
  return import.meta.env.VITE_API_BASE || "";
}

async function adminFetch(path, secret, opts = {}) {
  // Prefer session token when present (D3); fall back to ADMIN_SECRET / passed secret.
  const session =
    typeof localStorage !== "undefined"
      ? localStorage.getItem(SESSION_KEY)
      : null;
  const bearer = session || secret;
  const headers = {
    ...(opts.headers || {}),
    Authorization: `Bearer ${bearer}`,
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
  const [secret, setSecret] = useState(
    () =>
      localStorage.getItem(SESSION_KEY) ||
      localStorage.getItem(SECRET_KEY) ||
      "",
  );
  const [input, setInput] = useState(
    () => localStorage.getItem(SECRET_KEY) || "",
  );
  const [err, setErr] = useState("");
  const [stats, setStats] = useState(null);
  const [tokens, setTokens] = useState([]);
  const [keys, setKeys] = useState([]);
  const [settings, setSettings] = useState(null);
  const [nodes, setNodes] = useState([]);
  const [nodeHost, setNodeHost] = useState("127.0.0.1");
  const [nodePort, setNodePort] = useState("7890");
  const [nodeUser, setNodeUser] = useState("");
  const [nodePass, setNodePass] = useState("");
  const [tokenName, setTokenName] = useState("admin");
  const [newToken, setNewToken] = useState("");
  const [keyService, setKeyService] = useState("tavily");
  const [keyValue, setKeyValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [playToken, setPlayToken] = useState(
    () => localStorage.getItem(PLAY_TOKEN_KEY) || "",
  );
  const [playQuery, setPlayQuery] = useState("rust axum");
  const [playMax, setPlayMax] = useState("5");
  const [playResult, setPlayResult] = useState(null);
  const [playErr, setPlayErr] = useState("");

  const loggedIn = useMemo(() => Boolean(secret), [secret]);

  const refresh = useCallback(async (s) => {
    setBusy(true);
    setErr("");
    try {
      const [st, tk, ky, set, nd] = await Promise.all([
        adminFetch("/api/stats", s),
        adminFetch("/api/tokens", s),
        adminFetch("/api/keys", s),
        adminFetch("/api/settings", s),
        adminFetch("/api/nodes", s),
      ]);
      setStats(st);
      setTokens(tk || []);
      setKeys(ky || []);
      setSettings(set);
      setNodes(nd || []);
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
        <h2 style={{ marginTop: 0, fontSize: "1rem" }}>Settings</h2>
        {settings ? (
          <form onSubmit={saveSettings} className="row">
            <label className="row" style={{ gap: "0.35rem" }}>
              <input
                type="checkbox"
                checked={Boolean(settings.socialEnabled)}
                onChange={(e) =>
                  setSettings((prev) => ({ ...prev, socialEnabled: e.target.checked }))
                }
              />
              socialEnabled (research social leg)
            </label>
            <button type="submit" disabled={busy}>
              Save settings
            </button>
          </form>
        ) : (
          <p className="muted">Loading…</p>
        )}
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
          <>
            <p className="mono" style={{ wordBreak: "break-all" }}>
              New token (copy once): {newToken}
            </p>
            <button
              type="button"
              className="secondary"
              onClick={() => {
                setPlayToken(newToken);
                localStorage.setItem(PLAY_TOKEN_KEY, newToken);
              }}
            >
              Use in playground
            </button>
          </>
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

      <div className="card">
        <h2 style={{ marginTop: 0, fontSize: "1rem" }}>Outbound nodes</h2>
        <p className="muted">
          Optional HTTP proxies for Tavily/Firecrawl/Exa (xAI always direct). Boot resolves
          OUTBOUND_PROXY env first, else enabled node URL.
        </p>
        <form onSubmit={createNode} className="row">
          <input
            value={nodeHost}
            onChange={(e) => setNodeHost(e.target.value)}
            placeholder="host"
            required
          />
          <input
            value={nodePort}
            onChange={(e) => setNodePort(e.target.value)}
            placeholder="port"
            style={{ width: 88 }}
            required
          />
          <input
            value={nodeUser}
            onChange={(e) => setNodeUser(e.target.value)}
            placeholder="username (opt)"
          />
          <input
            type="password"
            value={nodePass}
            onChange={(e) => setNodePass(e.target.value)}
            placeholder="password (opt)"
          />
          <button type="submit" disabled={busy || !nodeHost.trim()}>
            Add node
          </button>
        </form>
        <table>
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
                <td>
                  <button type="button" className="secondary" onClick={() => deleteNode(n.id)}>
                    Delete
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="card">
        <h2 style={{ marginTop: 0, fontSize: "1rem" }}>Search playground</h2>
        <p className="muted">
          Calls POST /api/search with a client token (tok-…), not ADMIN_SECRET.
        </p>
        <form onSubmit={runPlayground}>
          <div className="row">
            <input
              className="mono"
              style={{ flex: 1, minWidth: 220 }}
              value={playToken}
              onChange={(e) => setPlayToken(e.target.value)}
              placeholder="tok-… API token"
              required
            />
          </div>
          <div className="row">
            <input
              style={{ flex: 1, minWidth: 180 }}
              value={playQuery}
              onChange={(e) => setPlayQuery(e.target.value)}
              placeholder="query"
              required
            />
            <input
              style={{ width: 72 }}
              value={playMax}
              onChange={(e) => setPlayMax(e.target.value)}
              placeholder="max"
            />
            <button type="submit" disabled={busy || !playToken.trim() || !playQuery.trim()}>
              Search
            </button>
          </div>
        </form>
        {playErr && <p className="err">{playErr}</p>}
        {playResult && (
          <pre className="pre mono">{JSON.stringify(playResult, null, 2)}</pre>
        )}
      </div>
    </div>
  );
}
