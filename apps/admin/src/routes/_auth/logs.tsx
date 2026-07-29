import { createFileRoute } from "@tanstack/react-router";

import { LogsPanel } from "@/features/logs/LogsPanel";

export const Route = createFileRoute("/_auth/logs")({
  component: LogsPanel,
});
