import React, { useState } from "react";

/**
 * Provider keys panel. Local seed/sync form fields; mutations via props.
 */
export function KeysPanel({
  keys = [],
  busy,
  onCreate,
  onToggle,
  onDelete,
  onSyncCredits,
}) {
  const [keyService, setKeyService] = useState("tavily");
  const [keyValue, setKeyValue] = useState("");
  const [syncService, setSyncService] = useState(""); // "" = all

  function handleCreate(e) {
    e.preventDefault();
    if (!keyValue) return;
    // Leave keyValue set: useAdminData.createKey swallows errors, so await+clear
    // would wipe a typed key on failure. Task 7: rethrow from mutation (or parent
    // wrapper) then clear only on success.
    onCreate({ service: keyService, key: keyValue });
  }

  function handleSync() {
    const payload = {};
    if (syncService) payload.service = syncService;
    onSyncCredits(payload);
  }

  return (
    <section className="panel" id="keys">
      <div className="panel__head">
        <h2 className="panel__title">Provider keys</h2>
      </div>
      <div className="panel__body">
        <form onSubmit={handleCreate} className="row">
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
            onClick={handleSync}
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
                      onClick={() => onToggle(k.id)}
                    >
                      Toggle
                    </button>
                    <button
                      type="button"
                      className="btn btn--danger btn--sm"
                      onClick={() => onDelete(k.id)}
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
  );
}
