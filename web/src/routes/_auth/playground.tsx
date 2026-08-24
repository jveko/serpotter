import { createFileRoute } from "@tanstack/react-router";

import { PlaygroundPanel } from "@/features/playground/PlaygroundPanel";

export const Route = createFileRoute("/_auth/playground")({
  component: PlaygroundPanel,
});
