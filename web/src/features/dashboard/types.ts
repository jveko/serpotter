/** Row from GET /api/spend/keys ('unknown' service when key deleted). */
export type SpendKeyRow = {
  keyId?: number | null;
  tokenName?: string | null;
  service: string;
  requests: number;
  cost: number;
};

/** Row from GET /api/spend/services. */
export type SpendServiceRow = {
  service: string;
  requests: number;
  cost: number;
};
