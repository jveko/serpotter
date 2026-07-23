import React, { useState } from "react";

/**
 * Outbound nodes panel. Local host/port/user/pass; mutations via props.
 * Toggle labels: Disable when enabled, Enable when disabled.
 */
export function NodesPanel({
  nodes = [],
  busy,
  onCreate,
  onToggle,
  onDelete,
}) {
  const [nodeHost, setNodeHost] = useState("127.0.0.1");
  const [nodePort, setNodePort] = useState("7890");
  const [nodeUser, setNodeUser] = useState("");
  const [nodePass, setNodePass] = useState("");

  async function handleCreate(e) {
    e.preventDefault();
    if (!nodeHost.trim()) return;
    try {
      await onCreate({
        host: nodeHost.trim(),
        port: nodePort,
        username: nodeUser,
        password: nodePass,
      });
      setNodePass("");
    } catch {
      // Keep password on failure (createNode rethrows after setErr).
    }
  }

  return (
    <section className="panel" id="nodes">
      <div className="panel__head">
        <h2 className="panel__title">Outbound nodes</h2>
      </div>
      <div className="panel__body">
        <p className="panel__lede">
          Optional HTTP proxies for Tavily/Firecrawl/Exa (xAI always direct).
          Boot resolves OUTBOUND_PROXY env first, else enabled node URL.
        </p>
        <form onSubmit={handleCreate} className="row">
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
                <th>consecutiveFails</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {nodes.length === 0 ? (
                <tr>
                  <td colSpan={8} className="empty">
                    No nodes
                  </td>
                </tr>
              ) : (
                nodes.map((n) => (
                  <tr key={n.id}>
                    <td>{n.id}</td>
                    <td className="mono">{n.host}</td>
                    <td>{n.port}</td>
                    <td className="mono">{n.username || "—"}</td>
                    <td>{n.enabled ? "yes" : "no"}</td>
                    <td>{n.inflight}</td>
                    <td>{n.consecutiveFails ?? 0}</td>
                    <td className="table__actions">
                      <button
                        type="button"
                        className="btn btn--secondary btn--sm"
                        onClick={() => onToggle(n.id)}
                      >
                        {n.enabled ? "Disable" : "Enable"}
                      </button>
                      <button
                        type="button"
                        className="btn btn--danger btn--sm"
                        onClick={() => onDelete(n.id)}
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>
    </section>
  );
}
