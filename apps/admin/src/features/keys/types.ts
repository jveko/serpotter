/** Row from GET /api/keys (admin list; key masked). */
export type KeyRow = {
  id: number;
  service: string;
  keyPreview: string;
  active: boolean;
  consecutiveFails: number;
  creditsRemaining?: number | null;
  creditsLimit?: number | null;
  usageSyncedAt?: string | null;
  inflight?: number | null;
  leaseUntil?: string | null;
  lastUsedAt?: string | null;
};

/** Per-key result from POST /api/keys/sync-credits. */
export type SyncKeyResult = {
  id?: string | number;
  ok?: boolean;
  remaining?: number | null;
  limit?: number | null;
  error?: string;
};

/** Report from POST /api/keys/sync-credits. */
export type SyncReport = {
  service?: string;
  synced?: number;
  errors?: number;
  results?: SyncKeyResult[];
};
