import { useQuery } from "@tanstack/react-query";

import { statsQueryOptions } from "./queries";

/**
 * Stats inventory panel: strip metrics + byService table.
 * Loads via TanStack Query — template for later panels.
 */
export function StatsPanel() {
  const { data, error, isPending, isFetching, refetch } = useQuery(statsQueryOptions);

  const byService = Array.isArray(data?.byService) ? data.byService : [];
  const errMsg = error instanceof Error ? error.message : error ? String(error) : null;

  let meta = "live";
  if (isPending) meta = "loading";
  else if (error) meta = "error";
  else if (isFetching) meta = "refreshing";

  return (
    <section className="panel" id="stats">
      <div className="panel__head">
        <h2 className="panel__title">Stats</h2>
        <span className="panel__meta">{meta}</span>
      </div>
      <div className="panel__body">
        {isPending && !data ? (
          <p className="empty" aria-busy="true">
            Loading…
          </p>
        ) : error && !data ? (
          <div className="banner" role="alert">
            <p className="banner__text err">{errMsg}</p>
            <button
              type="button"
              className="btn btn--secondary btn--sm"
              disabled={isFetching}
              data-state={isFetching ? "loading" : undefined}
              onClick={() => {
                void refetch();
              }}
            >
              Retry
            </button>
          </div>
        ) : data ? (
          <>
            {error && (
              <div className="banner" role="alert">
                <p className="banner__text err">{errMsg}</p>
                <button
                  type="button"
                  className="btn btn--secondary btn--sm"
                  disabled={isFetching}
                  data-state={isFetching ? "loading" : undefined}
                  onClick={() => {
                    void refetch();
                  }}
                >
                  Retry
                </button>
              </div>
            )}
            <div className="stat-strip">
              <div className="stat">
                <span className="stat__label">tokens</span>
                <span className="stat__value">{data.tokens}</span>
              </div>
              <div className="stat">
                <span className="stat__label">keys</span>
                <span className="stat__value">{data.apiKeys}</span>
              </div>
              <div className="stat">
                <span className="stat__label">active</span>
                <span className="stat__value">{data.activeApiKeys}</span>
              </div>
              <div className="stat">
                <span className="stat__label">nodes</span>
                <span className="stat__value">{data.nodes}</span>
              </div>
              <div className="stat">
                <span className="stat__label">schema</span>
                <span className="stat__value">{data.schemaVersion}</span>
              </div>
              <div className="stat">
                <span className="stat__label">requestLogs</span>
                <span className="stat__value">{data.requestLogs ?? 0}</span>
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
                        <td className="mono">{s.creditsRemaining ?? "—"}</td>
                        <td className="mono">{s.creditsLimit ?? "—"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        ) : (
          <p className="empty">No stats</p>
        )}
      </div>
    </section>
  );
}
