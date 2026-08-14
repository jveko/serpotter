/** Service row from GET /api/stats `byService`. */
export type ServiceStatsDto = {
  service: string;
  keys: number;
  active: number;
  creditsRemaining?: number | null;
  creditsLimit?: number | null;
};

/** Fields rendered by StatsPanel / Topbar from GET /api/stats. */
export type StatsDto = {
  tokens: number;
  apiKeys: number;
  activeApiKeys: number;
  nodes: number;
  schemaVersion: number;
  recentRequests: number;
  byService: ServiceStatsDto[];
};

/** Row from GET /api/usage (camelCase; daily per service+provider). */
export type UsageDailyDto = {
  service: string;
  providerUsed: string;
  date: string;
  requests: number;
  successes: number;
  errors: number;
  tokens: number;
  cost: number;
};
