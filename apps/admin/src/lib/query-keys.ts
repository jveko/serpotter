export const qk = {
  stats: {
    all: ["stats"] as const,
    summary: () => ["stats", "summary"] as const,
  },
  tokens: {
    all: ["tokens"] as const,
    list: () => ["tokens", "list"] as const,
  },
  keys: {
    all: ["keys"] as const,
    list: () => ["keys", "list"] as const,
  },
  settings: {
    all: ["settings"] as const,
    root: () => ["settings", "root"] as const,
  },
  nodes: {
    all: ["nodes"] as const,
    list: () => ["nodes", "list"] as const,
  },
  requestLogs: {
    all: ["request-logs"] as const,
    list: (f?: { limit?: number }) =>
      ["request-logs", "list", { limit: f?.limit ?? 50 }] as const,
  },
};
