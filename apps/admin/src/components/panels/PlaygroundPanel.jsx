import React, { useState } from "react";

/**
 * API playground. Controlled playToken; local mode + fields;
 * onSearch({ token, mode, query, maxResults, url, scrapeTopN }) — no adminFetch.
 */
export function PlaygroundPanel({
  playToken,
  onPlayTokenChange,
  playResult,
  playErr,
  busy,
  onSearch,
}) {
  const [mode, setMode] = useState("search");
  const [playQuery, setPlayQuery] = useState("rust axum");
  const [playMax, setPlayMax] = useState("5");
  const [playUrl, setPlayUrl] = useState("https://example.com");
  const [scrapeTopN, setScrapeTopN] = useState("");

  const tokenOk = Boolean(String(playToken ?? "").trim());
  const queryOk = Boolean(playQuery.trim());
  const urlOk = Boolean(playUrl.trim());
  const canSubmit =
    !busy &&
    tokenOk &&
    (mode === "extract" ? urlOk : queryOk);

  function handleSubmit(e) {
    e.preventDefault();
    if (!canSubmit) return;
    onSearch({
      token: playToken,
      mode,
      query: playQuery,
      maxResults: playMax,
      url: playUrl,
      scrapeTopN,
    });
  }

  const submitLabel =
    mode === "extract" ? "Extract" : mode === "research" ? "Research" : "Search";

  return (
    <section className="panel panel--graphite" id="playground">
      <div className="panel__head">
        <h2 className="panel__title">API playground</h2>
      </div>
      <div className="panel__body">
        <p className="panel__lede">
          Calls POST /api/search, /api/extract, or /api/research with a client
          token (tok-…), not ADMIN_SECRET.
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
            <label className="field">
              <span className="field__label">Mode</span>
              <select
                className="select"
                value={mode}
                onChange={(e) => setMode(e.target.value)}
              >
                <option value="search">search</option>
                <option value="extract">extract</option>
                <option value="research">research</option>
              </select>
            </label>
          </div>

          {mode === "extract" ? (
            <div className="row">
              <label className="field" style={{ flex: 1 }}>
                <span className="field__label">URL</span>
                <input
                  className="input"
                  value={playUrl}
                  onChange={(e) => setPlayUrl(e.target.value)}
                  placeholder="https://…"
                  required
                />
              </label>
              <button
                type="submit"
                className="btn btn--primary btn--sm"
                disabled={!canSubmit}
                data-state={busy ? "loading" : undefined}
              >
                {submitLabel}
              </button>
            </div>
          ) : (
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
                <span className="field__label">
                  {mode === "research" ? "Max (opt)" : "Max"}
                </span>
                <input
                  className="input input--narrow"
                  value={playMax}
                  onChange={(e) => setPlayMax(e.target.value)}
                  placeholder="max"
                />
              </label>
              {mode === "research" && (
                <label className="field">
                  <span className="field__label">scrapeTopN</span>
                  <input
                    className="input input--narrow"
                    value={scrapeTopN}
                    onChange={(e) => setScrapeTopN(e.target.value)}
                    placeholder="opt"
                  />
                </label>
              )}
              <button
                type="submit"
                className="btn btn--primary btn--sm"
                disabled={!canSubmit}
                data-state={busy ? "loading" : undefined}
              >
                {submitLabel}
              </button>
            </div>
          )}
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
