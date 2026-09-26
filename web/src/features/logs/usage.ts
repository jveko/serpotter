import type { RequestLogRow } from "./types";

/**
 * Per-request cost estimate in USD. Four decimals because a single LLM call
 * routinely costs fractions of a cent (0.0042), which `toFixed(2)` — the
 * whole-spend figure in KpiStrip — would round to $0.00.
 */
export function formatCost(costEst: number | null | undefined): string {
  if (costEst == null) return "—";
  return `$${costEst.toFixed(4)}`;
}

/**
 * Compact token triple in `in/out/total` order. A row that reports no usage
 * at all renders as the em dash; a row with a total but no split keeps the
 * dashes in place rather than inventing zeroes.
 */
export function formatTokens(row: RequestLogRow): string {
  if (row.inputTokens == null && row.outputTokens == null && row.totalTokens == null) return "—";
  const part = (v: number | null | undefined) => (v == null ? "–" : String(v));
  return `${part(row.inputTokens)}/${part(row.outputTokens)}/${part(row.totalTokens)}`;
}
