import { tryParseUtc } from "./utc";

/**
 * Compact relative timestamp for display ("now", "5m ago", "3h ago", "2d ago").
 * Server stamps are UTC but zone-less ("YYYY-MM-DD HH:MM:SS" from SQLite
 * `datetime('now')` / `utc_now_str()`), so they are normalized as UTC — reading
 * them as local time made a UTC+7 operator see every stamp 7h off.
 */
export function relativeTime(stamp: string): string {
  const parsed = tryParseUtc(stamp);
  // No value, not a real age: "65000d ago" for a missing stamp reads as fact.
  // tryParseUtc (not parseUtcTimestamp) so a genuine 1970 stamp still shows
  // its true age instead of being folded into this "—" case.
  if (parsed === null) return "—";
  const ms = Date.now() - parsed;
  const mins = Math.floor(ms / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  return `${Math.floor(hrs / 24)}d ago`;
}
