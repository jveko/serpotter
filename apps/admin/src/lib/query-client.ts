import { MutationCache, QueryCache, QueryClient } from "@tanstack/react-query";

import { showToast } from "@/components/ui/toast";

declare module "@tanstack/react-query" {
  interface Register {
    mutationMeta: {
      successMessage?: string;
      errorMessage?: string;
      /** Skip global mutation toasts (e.g. panel handles honesty copy). */
      silent?: boolean;
    };
  }
}

function statusOf(e: unknown): number | undefined {
  if (typeof e !== "object" || e === null || !("status" in e)) return undefined;
  const status = Reflect.get(e, "status");
  return typeof status === "number" ? status : undefined;
}

function isUnauthorized(e: unknown): boolean {
  return statusOf(e) === 401;
}

function errMessage(err: unknown, fallback?: string): string {
  if (fallback) return fallback;
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === "string" && err) return err;
  return "Request failed";
}

export function createAppQueryClient(handlers: { onUnauthorized: () => void }): QueryClient {
  let handling401 = false;
  const handle401 = () => {
    if (handling401) return;
    handling401 = true;
    try {
      handlers.onUnauthorized();
    } finally {
      queueMicrotask(() => {
        handling401 = false;
      });
    }
  };

  return new QueryClient({
    queryCache: new QueryCache({
      onError: (err) => {
        if (isUnauthorized(err)) handle401();
      },
    }),
    mutationCache: new MutationCache({
      onSuccess: (_data, _vars, _ctx, mutation) => {
        const meta = mutation.meta;
        if (meta?.silent) return;
        if (meta?.successMessage) {
          showToast({
            title: meta.successMessage,
            type: "success",
          });
        }
      },
      onError: (err, _vars, _ctx, mutation) => {
        if (isUnauthorized(err)) {
          handle401();
          return;
        }
        const meta = mutation.meta;
        if (meta?.silent) return;
        showToast({
          title: errMessage(err, meta?.errorMessage),
          type: "error",
        });
      },
    }),
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        gcTime: 5 * 60_000,
        retry: (n, err) => !isUnauthorized(err) && n < 2,
        refetchOnWindowFocus: true,
      },
      mutations: { retry: false },
    },
  });
}
