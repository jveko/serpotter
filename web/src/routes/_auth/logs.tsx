import { createFileRoute } from "@tanstack/react-router";

import { LogsPanel } from "@/features/logs/LogsPanel";

type LogsSearch = { requestId?: string; status?: string; errorKind?: string };

export const Route = createFileRoute("/_auth/logs")({
  validateSearch: (search: Record<string, unknown>): LogsSearch => ({
    requestId:
      typeof search.requestId === "string" && search.requestId ? search.requestId : undefined,
    status:
      typeof search.status === "string" && /^[245]$/.test(search.status)
        ? search.status
        : undefined,
    errorKind:
      typeof search.errorKind === "string" && search.errorKind ? search.errorKind : undefined,
  }),
  component: LogsRouteComponent,
});

function LogsRouteComponent() {
  const search = Route.useSearch();
  return (
    <LogsPanel
      initialRequestId={search.requestId}
      initialStatus={search.status}
      initialErrorKind={search.errorKind}
    />
  );
}
