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
  queryPreview?: string | null;
  requestId?: string | null;
  tokenName?: string | null;
  strategy?: string | null;
  providersConsulted?: string | null;
  attemptCount?: number | null;
  keyId?: number | null;
  nodeId?: number | null;
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
};
