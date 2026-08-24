import { createFileRoute } from "@tanstack/react-router";

import { TokensPanel } from "@/features/tokens/TokensPanel";

export const Route = createFileRoute("/_auth/tokens")({
  component: TokensPanel,
});
