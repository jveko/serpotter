import { createFileRoute, Link } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";

import { KpiStrip } from "@/features/dashboard/KpiStrip";
import { PoolHealth } from "@/features/dashboard/PoolHealth";
import { RecentActivity } from "@/features/dashboard/RecentActivity";
import { SpendLeaderboard } from "@/features/dashboard/SpendLeaderboard";
import { UsageChart } from "@/features/dashboard/UsageChart";
import { perDayByService, splitUsageWindows, windowTotals } from "@/features/dashboard/metrics";
import { spendKeysQueryOptions, spendServicesQueryOptions } from "@/features/dashboard/queries";
import { keysQueryOptions } from "@/features/keys/queries";
import { requestLogsQueryOptions } from "@/features/logs/queries";
import { nodesQueryOptions } from "@/features/nodes/queries";
import { SourceAlerts } from "@/features/dashboard/source-alerts";
import { aggregateRegion, dashboardPanelStatus } from "@/features/dashboard/status";
import { statsQueryOptions, usageQueryOptions } from "@/features/stats/queries";
import { usePublishPanelStatus } from "@/features/shell/panel-status";

type DashboardSearch = { days?: number };

const DAYS_CHOICES = [7, 14, 30, 90];

export const Route = createFileRoute("/_auth/dashboard")({
  validateSearch: (search: Record<string, unknown>): DashboardSearch => {
    const raw = Number(search.days);
    const days = DAYS_CHOICES.includes(raw) ? raw : undefined;
    return days ? { days } : {};
  },
  component: DashboardPage,
});

const WINDOW_DAYS_DEFAULT = 14;

function DashboardPage() {
  const { days = WINDOW_DAYS_DEFAULT } = Route.useSearch();

  const statsQ = useQuery(statsQueryOptions);
  const usageQ = useQuery(usageQueryOptions(days * 2)); // current + previous windows
  const spendKeysQ = useQuery(spendKeysQueryOptions());
  const spendSvcQ = useQuery(spendServicesQueryOptions());
  const keysQ = useQuery(keysQueryOptions);
  const nodesQ = useQuery(nodesQueryOptions);
  const activityQ = useQuery(requestLogsQueryOptions({ limit: 8 }));

  const { current, previous } = splitUsageWindows(usageQ.data ?? [], days);
  const totals = windowTotals(current);
  const prevTotals = previous.length > 0 ? windowTotals(previous) : null;

  // Each card names the queries it actually consumes, so a failed source can
  // never leave a card rendering zeroed or empty data as if it were real.
  // KpiStrip draws its totals from usageQ, so a usage failure is a KPI failure.
  const regions = [
    aggregateRegion("kpi", "KPI summary", [statsQ, usageQ]),
    aggregateRegion("usage", "Usage chart", [usageQ]),
    aggregateRegion("leaderboard", "Spend leaderboard", [spendKeysQ, spendSvcQ]),
    aggregateRegion("pool", "Pool health", [statsQ, keysQ, nodesQ]),
    aggregateRegion("activity", "Recent activity", [activityQ]),
  ];
  const status = dashboardPanelStatus(regions);
  usePublishPanelStatus(status.state, status.detail);

  const retryAll = () => {
    void statsQ.refetch();
    void usageQ.refetch();
    void spendKeysQ.refetch();
    void spendSvcQ.refetch();
    void keysQ.refetch();
    void nodesQ.refetch();
    void activityQ.refetch();
  };

  return (
    <section className="block" aria-labelledby="dashboard-window">
      <div className="block__head">
        <h2 className="block__title" id="dashboard-window">
          Usage window
        </h2>
        <nav className="window-picker" aria-label="Usage window">
          {DAYS_CHOICES.map((d) => (
            <Link
              key={d}
              to="/dashboard"
              search={{ days: d }}
              className={`window-picker__opt ${d === days ? "is-active" : ""}`}
            >
              {d}d
            </Link>
          ))}
        </nav>
      </div>

      <SourceAlerts regions={regions} onRetry={retryAll} />

      {statsQ.data && usageQ.data ? (
        <KpiStrip totals={totals} previousTotals={prevTotals} stats={statsQ.data} />
      ) : null}

      {usageQ.data ? <UsageChart data={perDayByService(current)} windowDays={days} /> : null}

      {spendKeysQ.data && spendSvcQ.data ? (
        <SpendLeaderboard keys={spendKeysQ.data} services={spendSvcQ.data} />
      ) : null}

      {statsQ.data && keysQ.data && nodesQ.data ? (
        <PoolHealth stats={statsQ.data} keys={keysQ.data} nodes={nodesQ.data} />
      ) : null}

      {activityQ.data ? <RecentActivity rows={activityQ.data} /> : null}
    </section>
  );
}
