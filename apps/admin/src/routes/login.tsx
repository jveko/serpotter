import { createFileRoute, redirect } from "@tanstack/react-router";

import { LoginPage } from "@/features/auth/LoginPage";
import { safeRedirectPath } from "@/lib/safe-redirect";

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>) => ({
    redirect: typeof search.redirect === "string" ? search.redirect : undefined,
  }),
  beforeLoad: ({ context, search }) => {
    if (context.auth.isAuthenticated) {
      throw redirect({ to: safeRedirectPath(search.redirect) });
    }
  },
  component: LoginPage,
});
