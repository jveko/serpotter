/** Row from GET /api/request-logs (camelCase, optionals skipped when null). */
export type RequestLogRow = {
  id: number;
  createdAt: string;
  path: string;
  method: string;
  status: number;
  service?: string | null;
  providerUsed?: string | null;
  durationMs?: number | null;
  errorKind?: string | null;
  /** Per-request usage from the LLM provider; absent when the row is not an LLM call. */
  inputTokens?: number | null;
  outputTokens?: number | null;
  totalTokens?: number | null;
  costEst?: number | null;
  /** Whether the response was served from the in-process response cache. */
  cacheHit: boolean;
  queryPreview?: string | null;
  requestId?: string | null;
  tokenName?: string | null;
  strategy?: string | null;
  providersConsulted?: string | null;
  attemptCount?: number | null;
  keyId?: number | null;
  nodeId?: number | null;
  /** `service:outcome[:upstreamStatus]` per completed attempt. */
  attemptOutcomes?: string | null;
  /** Upstream status of the last attempt that reported one. */
  lastUpstreamStatus?: number | null;
  /** Distinct attempted key ids, comma-joined. */
  keyIds?: string | null;
  /** `service:transition:keyId` per key-state transition. */
  keyTransitions?: string | null;
};

/** Server-side filters for GET /api/request-logs (camelCase query params). */
export type RequestLogFilters = {
  limit: number;
  offset?: number;
  status?: string;
  path?: string;
  service?: string;
  requestId?: string;
  tokenName?: string;
  errorKind?: string;
  lastUpstreamStatus?: string;
};
