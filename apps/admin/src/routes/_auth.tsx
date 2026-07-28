import { Outlet, createFileRoute, redirect } from "@tanstack/react-router";

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
  // Temporary shell — full chrome in Task 12
  component: () => <Outlet />,
});
