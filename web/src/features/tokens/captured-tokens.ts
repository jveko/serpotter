export type CapturedToken = { id: number; name: string; plaintext: string };

/** One-shot plaintexts captured at create time; session-scoped by design. */
const captured = new Map<number, CapturedToken>();

export function rememberCapturedToken(id: number, name: string, plaintext: string): void {
  captured.set(id, { id, name, plaintext });
}

export function listCapturedTokens(): CapturedToken[] {
  return [...captured.values()];
}

/** Drop a captured token (e.g. when its key is revoked) so it no longer
 *  appears in the playground picker. */
export function forgetCapturedToken(id: number): void {
  captured.delete(id);
}
