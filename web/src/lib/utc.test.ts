import { describe, expect, it } from "vitest";

import { isoUtcTimestamp, parseUtcTimestamp } from "./utc";

describe("parseUtcTimestamp (zone-less server stamps are UTC)", () => {
  it("reads the SQLite datetime('now') shape as UTC, not local time", () => {
    // Backend writes "YYYY-MM-DD HH:MM:SS" (no T, no zone). new Date() would
    // read that as local — for a UTC+7 operator every stamp is 7h off.
    expect(parseUtcTimestamp("2026-09-25 12:00:00")).toBe(Date.parse("2026-09-25T12:00:00Z"));
  });

  it("reads a zone-less ISO stamp as UTC too", () => {
    expect(parseUtcTimestamp("2026-09-25T12:00:00")).toBe(Date.parse("2026-09-25T12:00:00Z"));
  });

  it("leaves zoned ISO-8601 values to the platform", () => {
    expect(parseUtcTimestamp("2026-09-25T12:00:00Z")).toBe(Date.parse("2026-09-25T12:00:00Z"));
    expect(parseUtcTimestamp("2026-09-25T12:00:00+07:00")).toBe(
      Date.parse("2026-09-25T12:00:00+07:00"),
    );
  });

  it("returns 0 for empty or unparseable values", () => {
    expect(parseUtcTimestamp("")).toBe(0);
    expect(parseUtcTimestamp(null)).toBe(0);
    expect(parseUtcTimestamp(undefined)).toBe(0);
    expect(parseUtcTimestamp("not-a-date")).toBe(0);
  });
});

describe("isoUtcTimestamp", () => {
  it("emits a valid ISO string for a zone-less server stamp", () => {
    expect(isoUtcTimestamp("2026-09-25 12:00:00")).toBe("2026-09-25T12:00:00.000Z");
  });

  it("emits a real ISO string for the Unix epoch — epoch is not a failure sentinel", () => {
    expect(isoUtcTimestamp("1970-01-01 00:00:00")).toBe("1970-01-01T00:00:00.000Z");
  });

  it("returns null for empty or unparseable stamps so callers omit the attribute", () => {
    expect(isoUtcTimestamp("")).toBeNull();
    expect(isoUtcTimestamp(null)).toBeNull();
    expect(isoUtcTimestamp(undefined)).toBeNull();
    expect(isoUtcTimestamp("not-a-date")).toBeNull();
  });
});
