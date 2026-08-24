import { createFileRoute } from "@tanstack/react-router";

import { SettingsPanel } from "@/features/settings/SettingsPanel";

export const Route = createFileRoute("/_auth/settings")({
  component: SettingsPanel,
});
