import { createFileRoute } from "@tanstack/react-router";

import { NodesPanel } from "@/features/nodes/NodesPanel";

export const Route = createFileRoute("/_auth/nodes")({
  component: NodesPanel,
});
