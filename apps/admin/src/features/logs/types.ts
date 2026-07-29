/** Row from GET /api/request-logs (camelCase). */
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
};
