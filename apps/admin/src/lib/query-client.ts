import {
  MutationCache,
  QueryCache,
  QueryClient,
} from "@tanstack/react-query";

export function createAppQueryClient(handlers: {
  onUnauthorized: () => void;
}): QueryClient {
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
  const isUnauthorized = (e: unknown) =>
    typeof e === "object" &&
    e !== null &&
    "status" in e &&
    (e as { status: number }).status === 401;

  return new QueryClient({
    queryCache: new QueryCache({
      onError: (err) => {
        if (isUnauthorized(err)) handle401();
      },
    }),
    mutationCache: new MutationCache({
      onError: (err) => {
        if (isUnauthorized(err)) handle401();
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
