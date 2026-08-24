import { createFileRoute } from "@tanstack/react-router";

import { KeysPanel } from "@/features/keys/KeysPanel";

export const Route = createFileRoute("/_auth/keys")({
  validateSearch: (search: Record<string, unknown>): { focus?: number } => {
    const raw = Number(search.focus);
    return Number.isInteger(raw) && raw > 0 ? { focus: raw } : {};
  },
  component: KeysPanel,
});
