import { createFileRoute, redirect } from "@tanstack/react-router";

import { LoginPage } from "@/features/auth/LoginPage";
import { getAuthSnapshot } from "@/features/auth/auth-snapshot";
import { safeRedirectPath } from "@/lib/safe-redirect";

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>) => ({
    redirect: typeof search.redirect === "string" ? search.redirect : undefined,
  }),
  beforeLoad: ({ search }) => {
    // Prefer module snapshot over context.auth so logout/401 navigate same-turn is correct.
    if (getAuthSnapshot().isAuthenticated) {
      throw redirect({ to: safeRedirectPath(search.redirect) });
    }
  },
  component: LoginPage,
});
