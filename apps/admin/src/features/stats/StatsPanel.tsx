import { useQuery } from "@tanstack/react-query";

import { usePublishPanelStatus } from "@/features/shell/panel-status";

import { statsQueryOptions } from "./queries";

/**
 * Stats inventory: full-bleed metrics strip + byService table.
 * Loads via TanStack Query — template for later panels.
 */
export function StatsPanel() {
  const { data, error, isPending, isFetching, refetch } = useQuery(statsQueryOptions);

  const byService = Array.isArray(data?.byService) ? data.byService : [];
  const errMsg = error instanceof Error ? error.message : error ? String(error) : null;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (isFetching) state = "refreshing";

  usePublishPanelStatus(
    state,
    data ? `${byService.length} services · schema ${data.schemaVersion}` : undefined,
  );

  if (isPending && !data) {
    return (
      <p className="empty" aria-busy="true">
        Loading…
      </p>
    );
  }

  if (error && !data) {
    return (
      <div className="block">
        <p className="err" role="alert">
          {errMsg}
        </p>
        <div className="row">
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            disabled={isFetching}
            data-state={isFetching ? "loading" : undefined}
            onClick={() => void refetch()}
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (!data) return <p className="empty">No stats</p>;

  return (
    <>
      {error ? (
        <p className="err" role="alert">
          {errMsg}
        </p>
      ) : null}

      <div className="metrics bleed" id="stats">
        <div className="metric">
          <span className="metric__label">tokens</span>
          <span className="metric__value">{data.tokens}</span>
        </div>
        <div className="metric">
          <span className="metric__label">keys</span>
          <span className="metric__value">{data.apiKeys}</span>
        </div>
        <div className="metric">
          <span className="metric__label">active</span>
          <span className="metric__value">{data.activeApiKeys}</span>
        </div>
        <div className="metric">
          <span className="metric__label">nodes</span>
          <span className="metric__value">{data.nodes}</span>
        </div>
        <div className="metric">
          <span className="metric__label">request logs</span>
          <span className="metric__value">{data.requestLogs ?? 0}</span>
        </div>
      </div>

      {byService.length > 0 && (
        <section className="block" aria-labelledby="stats-by-service">
          <div className="block__head">
            <h2 className="block__title" id="stats-by-service">
              By service
            </h2>
            <p className="block__note">Provider key counts and synced credit balances.</p>
          </div>
          <div className="table-scroll bleed">
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
        </section>
      )}
    </>
  );
}
