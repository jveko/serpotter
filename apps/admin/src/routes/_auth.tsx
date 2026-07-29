import { Outlet, createFileRoute, redirect } from "@tanstack/react-router";

import { getAuthSnapshot } from "@/features/auth/auth-snapshot";
import { Shell } from "@/features/shell/Shell";
import { safeRedirectPath } from "@/lib/safe-redirect";

export const Route = createFileRoute("/_auth")({
  beforeLoad: ({ location }) => {
    // Prefer module snapshot over context.auth so logout/401 navigate same-turn is correct.
    if (!getAuthSnapshot().isAuthenticated) {
      throw redirect({
        to: "/login",
        search: {
          // location.pathname is already basepath-stripped
          redirect: safeRedirectPath(location.pathname),
        },
      });
    }
  },
  component: AuthLayout,
});

function AuthLayout() {
  return (
    <Shell>
      <Outlet />
    </Shell>
  );
}
