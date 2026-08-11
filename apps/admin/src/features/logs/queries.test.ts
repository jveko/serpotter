import { afterEach, describe, expect, it, vi } from "vitest";

import { buildRequestLogsUrl, FilterDebouncer, withFilter } from "./queries";

describe("buildRequestLogsUrl", () => {
  it("always includes limit and skips blank filters", () => {
    expect(buildRequestLogsUrl({ limit: 25 })).toBe("/api/request-logs?limit=25");
  });

  it("serializes non-blank filters, trimming values", () => {
    const url = buildRequestLogsUrl({
      limit: 50,
      status: "200",
      path: "/api/se",
      service: "",
      requestId: "req-1",
    });
    expect(url).toBe("/api/request-logs?limit=50&status=200&path=%2Fapi%2Fse&requestId=req-1");
  });
});

describe("withFilter", () => {
  it("sets a filter and trims its value", () => {
    const f = withFilter({ limit: 50 }, "path", "  /api/se  ");
    expect(f.path).toBe("/api/se");
    expect(f.limit).toBe(50);
  });

  it("removes the field when blank", () => {
    const f = withFilter({ limit: 50, status: "200" }, "status", "   ");
    expect("status" in f).toBe(false);
  });
});

describe("FilterDebouncer (LogsPanel per-keystroke fetch)", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("collapses rapid filter changes into a single commit", () => {
    vi.useFakeTimers();
    const commit = vi.fn();
    const d = new FilterDebouncer(commit, 300);
    d.push(withFilter({ limit: 50 }, "path", "a"));
    d.push(withFilter({ limit: 50 }, "path", "ab"));
    d.push(withFilter({ limit: 50 }, "path", "abc"));
    expect(commit).not.toHaveBeenCalled();
    vi.advanceTimersByTime(299);
    expect(commit).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(commit).toHaveBeenCalledTimes(1);
    expect(commit).toHaveBeenCalledWith({ limit: 50, path: "abc" });
  });

  it("commits after the quiet window and can be cancelled", () => {
    vi.useFakeTimers();
    const commit = vi.fn();
    const d = new FilterDebouncer(commit, 300);
    d.push(withFilter({ limit: 50 }, "status", "200"));
    d.cancel();
    vi.advanceTimersByTime(400);
    expect(commit).not.toHaveBeenCalled();
  });
});
