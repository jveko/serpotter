import { useEffect, useState } from "react";

import { usePublishPanelStatus } from "@/features/shell/panel-status";
import { PLAY_TOKEN_KEY } from "@/lib/constants";

import { runPlayground } from "./runPlayground";

/**
 * API playground. playToken from PLAY_TOKEN_KEY; local mode + fields;
 * runPlayground on submit — no adminFetch.
 */
export function PlaygroundPanel() {
  const [playToken, setPlayToken] = useState(() => localStorage.getItem(PLAY_TOKEN_KEY) || "");
  useEffect(() => {
    const sync = () => setPlayToken(localStorage.getItem(PLAY_TOKEN_KEY) || "");
    window.addEventListener("serpotter:play-token", sync);
    return () => window.removeEventListener("serpotter:play-token", sync);
  }, []);

  const [mode, setMode] = useState("search");
  const [playQuery, setPlayQuery] = useState("rust axum");
  const [playMax, setPlayMax] = useState("5");
  const [playUrl, setPlayUrl] = useState("https://example.com");
  const [scrapeTopN, setScrapeTopN] = useState("");

  const [playResult, setPlayResult] = useState<unknown>(null);
  const [playStatus, setPlayStatus] = useState<number | null>(null);
  const [playErr, setPlayErr] = useState("");
  const [pending, setPending] = useState(false);

  const tokenOk = Boolean(String(playToken ?? "").trim());
  const queryOk = Boolean(playQuery.trim());
  const urlOk = Boolean(playUrl.trim());
  const canSubmit = !pending && tokenOk && (mode === "extract" ? urlOk : queryOk);

  let state = "ready";
  if (pending) state = "calling";
  else if (playErr) state = "error";
  else if (playResult != null) state = "ok";

  usePublishPanelStatus(state, `${mode} · ${tokenOk ? "token set" : "no token"}`);

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;
    setPlayErr("");
    setPlayResult(null);
    setPlayStatus(null);
    setPending(true);
    const out = await runPlayground({
      token: playToken,
      mode,
      query: playQuery,
      maxResults: playMax,
      url: playUrl,
      scrapeTopN,
    });
    setPending(false);
    if (out.ok) {
      setPlayStatus(out.status);
      setPlayResult(out.data);
      setPlayErr("");
    } else {
      setPlayStatus(out.status);
      setPlayErr(out.error);
      setPlayResult(null);
    }
  }

  const submitLabel = mode === "extract" ? "Extract" : mode === "research" ? "Research" : "Search";

  return (
    <>
      <section className="block" id="playground" aria-labelledby="playground-request">
        <div className="block__head">
          <h2 className="block__title" id="playground-request">
            Request
          </h2>
          <p className="block__note">
            Calls <span className="mono">POST /api/search</span>,{" "}
            <span className="mono">/api/extract</span> or{" "}
            <span className="mono">/api/research</span> as a client, with a{" "}
            <span className="mono">tok-…</span> token — never ADMIN_SECRET.
          </p>
        </div>
        <form onSubmit={handleSubmit} className="block">
          <div className="row">
            <label className="field field--grow">
              <span className="field__label">API token</span>
              <input
                className="input input--mono"
                value={playToken}
                onChange={(e) => setPlayToken(e.target.value)}
                placeholder="tok-… API token"
                required
              />
            </label>
            <label className="field">
              <span className="field__label">Mode</span>
              <select className="select" value={mode} onChange={(e) => setMode(e.target.value)}>
                <option value="search">search</option>
                <option value="extract">extract</option>
                <option value="research">research</option>
              </select>
            </label>
          </div>

          {mode === "extract" ? (
            <div className="row">
              <label className="field field--grow">
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
                data-state={pending ? "loading" : undefined}
              >
                {submitLabel}
              </button>
            </div>
          ) : (
            <div className="row">
              <label className="field field--grow">
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
                <span className="field__label">{mode === "research" ? "Max (opt)" : "Max"}</span>
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
                data-state={pending ? "loading" : undefined}
              >
                {submitLabel}
              </button>
            </div>
          )}
        </form>
      </section>

      {(playErr || playResult != null) && (
        <section className="block" aria-labelledby="playground-response">
          <div className="row row--spread">
            <h2 className="block__title" id="playground-response">
              Response
            </h2>
            {playStatus != null ? (
              <span className={playErr ? "chip chip--warn" : "chip chip--ok"}>
                {playStatus} {playErr ? "error" : "OK"}
              </span>
            ) : null}
          </div>
          {playErr ? (
            <p className="err" role="alert">
              {playErr}
            </p>
          ) : (
            <pre className="pre">{JSON.stringify(playResult, null, 2)}</pre>
          )}
        </section>
      )}
    </>
  );
}
