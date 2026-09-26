/**
 * Server timestamps are UTC but zone-less. The backend writes them two ways:
 * SQLite `datetime('now', …)` columns and `utc_now_str()` both emit
 * "YYYY-MM-DD HH:MM:SS" (no `T`, no zone designator), and
 * `new Date("2026-09-25 12:00:00")` reads that shape as LOCAL time — a
 * UTC+7 operator sees every stamp 7h off, and a `<time dateTime>` attribute
 * receives a non-ISO value. ISO-8601 inputs (with `Z` or an offset) pass
 * through unchanged; zone-less ISO is treated as UTC per the backend contract.
 */

/** Insert the `T` separator a zone-less stamp lacks, and `Z` if it has no zone. */
function toUtcIso(value: string): string {
  const withT = value.includes("T") ? value : value.replace(" ", "T");
  return /(?:Z|[+-]\d{2}:\d{2})$/.test(withT) ? withT : `${withT}Z`;
}

/**
 * Epoch ms, or null when the stamp is empty or unparseable. This is the one
 * place a server stamp is decoded: callers that must tell "no value" from the
 * Unix epoch (a real, representable instant) branch on null here rather than
 * on the 0 sentinel `parseUtcTimestamp` returns.
 */
export function tryParseUtc(value: string | null | undefined): number | null {
  if (!value) return null;
  const t = Date.parse(toUtcIso(value));
  return Number.isNaN(t) ? null : t;
}

/**
 * Parse a server timestamp as UTC epoch ms. Returns 0 for empty/unparseable
 * values (never NaN) — for arithmetic that has no failure channel; use
 * `isoUtcTimestamp` when a parse failure must stay distinguishable.
 */
export function parseUtcTimestamp(value: string | null | undefined): number {
  return tryParseUtc(value) ?? 0;
}

/**
 * The same instant as a full ISO-8601 UTC string, for `<time dateTime>` and
 * other machine-readable surfaces. Returns null when the stamp is empty or
 * unparseable — a null is NOT the Unix epoch ("1970-01-01 00:00:00" is a
 * real, representable instant), so callers branch on null:
 *
 *     <time dateTime={isoUtcTimestamp(row.createdAt) ?? undefined}>
 *
 * `undefined` omits the attribute entirely; "" would still render
 * `dateTime=""`, an invalid date string.
 */
export function isoUtcTimestamp(value: string | null | undefined): string | null {
  const ms = tryParseUtc(value);
  return ms === null ? null : new Date(ms).toISOString();
}
