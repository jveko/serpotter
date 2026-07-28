/** Row from GET /api/tokens (list masks middle). */
export type TokenRow = {
  id: number;
  name: string;
  /** Full token only on create. */
  token?: string;
  tokenPreview?: string | null;
  createdAt: string;
};

/** POST /api/tokens create response (one-shot plaintext). */
export type CreateTokenResult = TokenRow & {
  token?: string;
};
