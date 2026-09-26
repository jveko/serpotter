import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { relativeTime } from "./relative-time";

// TZ must be a NON-UTC zone for this suite to mean anything: on a UTC runner
// the pre-fix `new Date("YYYY-MM-DD HH:MM:SS")` (read as LOCAL time) resolves
// to the same instant as the correct UTC read, so no assertion here can
// discriminate and the P2 regression would go unguarded. process.env.TZ is
// process-scoped and a worker may run several test files, so the prior value
// is captured and restored in afterAll; the first test re-checks the pin
// actually took effect rather than trusting it.
const priorTZ = process.env.TZ;
process.env.TZ = "Asia/Bangkok";

const NOW = Date.parse("2026-09-25T12:00:00Z");

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(NOW);
});

afterEach(() => {
  vi.useRealTimers();
});

afterAll(() => {
  if (priorTZ === undefined) delete process.env.TZ;
  else process.env.TZ = priorTZ;
});

describe("relativeTime under a non-UTC runner timezone", () => {
  it("runs in a zone that can distinguish UTC from local parsing", () => {
    // Guard the guard: if the pin ever silently fails, every other assertion
    // here becomes vacuous and the regression returns undetected.
    expect(new Date("2026-09-25 12:00:00").toISOString()).not.toBe("2026-09-25T12:00:00.000Z");
  });

  it("reads a space-separated UTC stamp as UTC, not local time", () => {
    // Local-time parsing would place this 7h earlier, i.e. "16h ago" — a
    // different bucket, so this assertion fails on the old implementation.
    expect(relativeTime("2026-09-25 03:00:00")).toBe("9h ago");
    expect(relativeTime("2026-09-25 10:30:00")).toBe("1h ago");
  });

  it("agrees between the zone-less and the explicit-Z form", () => {
    expect(relativeTime("2026-09-25 03:00:00")).toBe(relativeTime("2026-09-25T03:00:00Z"));
  });

  it("covers all four display buckets", () => {
    // Each tier of the chain, so a dropped or reworded branch cannot pass
    // unnoticed — the other cases only exercise the "Nh ago" tier.
    expect(relativeTime("2026-09-25 11:59:30")).toBe("now");
    expect(relativeTime("2026-09-25 11:55:00")).toBe("5m ago");
    expect(relativeTime("2026-09-25 09:00:00")).toBe("3h ago");
    expect(relativeTime("2026-09-23 12:00:00")).toBe("2d ago");
  });

  it("renders an em dash, not a bogus age, for an empty or unparseable stamp", () => {
    expect(relativeTime("")).toBe("—");
    expect(relativeTime("not-a-date")).toBe("—");
  });

  it("shows a genuine 1970 stamp its real age rather than the em dash", () => {
    // The epoch is a representable instant, not a parse failure.
    expect(relativeTime("1970-01-01 00:00:00")).toBe("20721d ago");
  });
});
