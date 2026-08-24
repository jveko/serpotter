import { createFileRoute } from "@tanstack/react-router";

import { StatsPanel } from "@/features/stats/StatsPanel";

export const Route = createFileRoute("/_auth/stats")({
  component: StatsPanel,
});
