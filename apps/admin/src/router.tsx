import { createRouter } from "@tanstack/react-router";
import type { QueryClient } from "@tanstack/react-query";

import type { AuthContextValue } from "@/features/auth/types";

import { routeTree } from "./routeTree.gen";

export type RouterContext = {
  auth: AuthContextValue;
  queryClient: QueryClient;
};

export const router = createRouter({
  routeTree,
  defaultPreload: "intent",
  context: {
    auth: undefined!,
    queryClient: undefined!,
  },
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
