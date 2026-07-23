import React, { useState } from "react";

/**
 * Search playground. Controlled playToken; local query/max;
 * onSearch({ token, query, maxResults }) — no adminFetch.
 */
export function PlaygroundPanel({
  playToken,
  onPlayTokenChange,
  playResult,
  playErr,
  busy,
  onSearch,
}) {
  const [playQuery, setPlayQuery] = useState("rust axum");
  const [playMax, setPlayMax] = useState("5");

  function handleSubmit(e) {
    e.preventDefault();
    onSearch({
      token: playToken,
      query: playQuery,
      maxResults: playMax,
    });
  }

  return (
    <section className="panel panel--graphite" id="playground">
      <div className="panel__head">
        <h2 className="panel__title">Search playground</h2>
      </div>
      <div className="panel__body">
        <p className="panel__lede">
          Calls POST /api/search with a client token (tok-…), not ADMIN_SECRET.
        </p>
        <form onSubmit={handleSubmit}>
          <div className="row">
            <label className="field" style={{ flex: 1 }}>
              <span className="field__label">API token</span>
              <input
                className="input input--mono"
                value={playToken}
                onChange={(e) => onPlayTokenChange(e.target.value)}
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
              disabled={busy || !String(playToken ?? "").trim() || !playQuery.trim()}
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
            <pre className="pre mono">{JSON.stringify(playResult, null, 2)}</pre>
          </div>
        )}
      </div>
    </section>
  );
}
