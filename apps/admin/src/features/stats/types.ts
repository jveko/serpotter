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
  requestLogs: number;
  byService: ServiceStatsDto[];
};
