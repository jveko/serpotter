import { Outlet, createFileRoute, redirect } from "@tanstack/react-router";

import { Shell } from "@/features/shell/Shell";
import { safeRedirectPath } from "@/lib/safe-redirect";

export const Route = createFileRoute("/_auth")({
  beforeLoad: ({ context, location }) => {
    if (!context.auth.isAuthenticated) {
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
