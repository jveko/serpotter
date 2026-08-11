/** Row from GET /api/nodes (admin list). */
export type NodeRow = {
  id: number;
  host: string;
  port: number;
  protocol: string;
  enabled: boolean;
  inflight: number;
  consecutiveFails: number;
  username?: string | null;
  lastError?: string | null;
  leaseUntil?: string | null;
};

/** Result of a live connectivity probe (POST /api/nodes/{id}/test). */
export type NodeTestResult = {
  ok: boolean;
  latencyMs?: number | null;
  error?: string | null;
};
