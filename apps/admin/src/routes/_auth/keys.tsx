import { createFileRoute } from "@tanstack/react-router";

import { KeysPanel } from "@/features/keys/KeysPanel";

export const Route = createFileRoute("/_auth/keys")({
  component: KeysPanel,
});
