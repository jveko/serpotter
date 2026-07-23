import React, { useState } from "react";

/**
 * API tokens panel. Local name field; mutations via props.
 * onUseInPlayground(token) only — no jumpTo.
 */
export function TokensPanel({
  tokens = [],
  newToken,
  busy,
  onCreate,
  onDelete,
  onUseInPlayground,
}) {
  const [tokenName, setTokenName] = useState("admin");

  function handleCreate(e) {
    e.preventDefault();
    onCreate({ name: tokenName });
  }

  return (
    <section className="panel" id="tokens">
      <div className="panel__head">
        <h2 className="panel__title">API tokens</h2>
      </div>
      <div className="panel__body">
        <form onSubmit={handleCreate} className="row">
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
            <p className="mono break">New token (copy once): {newToken}</p>
            <button
              type="button"
              className="btn btn--secondary btn--sm"
              onClick={() => onUseInPlayground(newToken)}
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
                <th>createdAt</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {tokens.length === 0 ? (
                <tr>
                  <td colSpan={5} className="empty">
                    No tokens
                  </td>
                </tr>
              ) : (
                tokens.map((t) => (
                  <tr key={t.id}>
                    <td>{t.id}</td>
                    <td>{t.name}</td>
                    <td className="mono">{t.tokenPreview}</td>
                    <td className="mono">{t.createdAt || "—"}</td>
                    <td className="table__actions">
                      <button
                        type="button"
                        className="btn btn--secondary btn--sm"
                        onClick={() => onDelete(t.id)}
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
