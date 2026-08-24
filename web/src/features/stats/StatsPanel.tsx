import { useState } from "react";
import { useQuery } from "@tanstack/react-query";

import { usePublishPanelStatus } from "@/features/shell/panel-status";

import { statsQueryOptions, usageQueryOptions } from "./queries";

const USAGE_DAY_OPTIONS = [7, 14, 30, 90] as const;

/**
 * Stats inventory: full-bleed metrics strip + byService table + daily usage
 * table (GET /api/usage). Loads via TanStack Query — template for later panels.
 */
export function StatsPanel() {
  const { data, error, isPending, isFetching, refetch } = useQuery(statsQueryOptions);
  const [usageDays, setUsageDays] = useState<number>(14);
  const usageQuery = useQuery(usageQueryOptions(usageDays));

  const byService = Array.isArray(data?.byService) ? data.byService : [];
  const usageRows = Array.isArray(usageQuery.data) ? usageQuery.data : [];
  const errMsg = error instanceof Error ? error.message : error ? String(error) : null;
  const usageErrMsg =
    usageQuery.error instanceof Error
      ? usageQuery.error.message
      : usageQuery.error
        ? String(usageQuery.error)
        : null;

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
          <span className="metric__label">recent requests</span>
          <span className="metric__value">{data.recentRequests ?? 0}</span>
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

      <section className="block" aria-labelledby="stats-usage">
        <div className="block__head">
          <h2 className="block__title" id="stats-usage">
            Daily usage
          </h2>
          <p className="block__note">
            Requests / success / error / token / cost from <span className="mono">/api/usage</span>{" "}
            (usage_daily, upserted per request).
          </p>
          <label className="field">
            <span className="field__label">Days</span>
            <select
              className="input"
              value={usageDays}
              onChange={(e) => setUsageDays(Number(e.target.value))}
            >
              {USAGE_DAY_OPTIONS.map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </label>
        </div>
        {usageErrMsg ? (
          <p className="err" role="alert">
            {usageErrMsg}
          </p>
        ) : null}
        <div className="table-scroll bleed">
          <table className="table">
            <thead>
              <tr>
                <th>date</th>
                <th>service</th>
                <th>providerUsed</th>
                <th>requests</th>
                <th>successes</th>
                <th>errors</th>
                <th>tokens</th>
                <th>cost</th>
              </tr>
            </thead>
            <tbody>
              {usageRows.length === 0 ? (
                <tr>
                  <td colSpan={8} className="empty">
                    {usageQuery.isPending && !usageQuery.data
                      ? "Loading…"
                      : "No usage rows yet (written per request; send a search first)"}
                  </td>
                </tr>
              ) : (
                usageRows.map((u) => (
                  <tr key={`${u.date}-${u.service}-${u.providerUsed}`}>
                    <td className="mono">{u.date}</td>
                    <td>{u.service}</td>
                    <td>{u.providerUsed}</td>
                    <td>{u.requests}</td>
                    <td>{u.successes}</td>
                    <td>{u.errors}</td>
                    <td>{u.tokens}</td>
                    <td className="mono">{u.cost.toFixed(4)}</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </section>
    </>
  );
}
