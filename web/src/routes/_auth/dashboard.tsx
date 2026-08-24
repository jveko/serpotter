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
import { statsQueryOptions, usageQueryOptions } from "@/features/stats/queries";

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

  return (
    <section className="page page--dashboard" aria-label="Dashboard">
      <header className="page__head">
        <h2>Dashboard</h2>
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
      </header>

      {statsQ.data ? (
        <KpiStrip totals={totals} previousTotals={prevTotals} stats={statsQ.data} />
      ) : null}

      <UsageChart data={perDayByService(current)} windowDays={days} />

      {spendKeysQ.data && spendSvcQ.data ? (
        <SpendLeaderboard keys={spendKeysQ.data} services={spendSvcQ.data} />
      ) : null}

      {statsQ.data && keysQ.data && nodesQ.data ? (
        <PoolHealth stats={statsQ.data} keys={keysQ.data} nodes={nodesQ.data} />
      ) : null}

      <RecentActivity rows={activityQ.data ?? []} />
    </section>
  );
}
