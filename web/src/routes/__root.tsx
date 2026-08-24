import { Link, Outlet, createRootRouteWithContext } from "@tanstack/react-router";
import type { ErrorComponentProps } from "@tanstack/react-router";
import { TanStackRouterDevtools } from "@tanstack/react-router-devtools";

import type { RouterContext } from "@/router";

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootComponent,
  errorComponent: RootErrorComponent,
  notFoundComponent: RootNotFoundComponent,
});

function RootComponent() {
  return (
    <>
      <Outlet />
      {import.meta.env.DEV ? <TanStackRouterDevtools position="bottom-right" /> : null}
    </>
  );
}

function RootErrorComponent({ error, reset }: ErrorComponentProps) {
  return (
    <main className="block">
      <div className="block__head">
        <h1 className="block__title">Something went wrong</h1>
      </div>
      <p className="err" role="alert">
        {error instanceof Error ? error.message : String(error)}
      </p>
      <div className="row">
        <button type="button" className="btn btn--primary btn--sm" onClick={() => reset()}>
          Retry
        </button>
        <Link to="/dashboard" className="btn btn--secondary btn--sm">
          Go home
        </Link>
      </div>
    </main>
  );
}

function RootNotFoundComponent() {
  return (
    <main className="block">
      <div className="block__head">
        <h1 className="block__title">Page not found</h1>
      </div>
      <p className="empty">The page you requested does not exist.</p>
      <div className="row">
        <Link to="/dashboard" className="btn btn--primary btn--sm">
          Go home
        </Link>
      </div>
    </main>
  );
}
