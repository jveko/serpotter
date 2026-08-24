import { afterEach, describe, expect, it, vi } from "vitest";

import {
  buildRequestLogsUrl,
  clampOffset,
  FilterDebouncer,
  nextPage,
  prevPage,
  resetToFirstPage,
  withFilter,
} from "./queries";

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
      tokenName: "tok-local",
    });
    expect(url).toBe(
      "/api/request-logs?limit=50&status=200&path=%2Fapi%2Fse&requestId=req-1&tokenName=tok-local",
    );
  });

  it("serializes a nonzero offset and omits offset 0", () => {
    expect(buildRequestLogsUrl({ limit: 25, offset: 0 })).toBe("/api/request-logs?limit=25");
    expect(buildRequestLogsUrl({ limit: 25, offset: 50 })).toBe(
      "/api/request-logs?limit=25&offset=50",
    );
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

  it("handles tokenName like any other filter", () => {
    const f = withFilter({ limit: 50 }, "tokenName", " tok-a ");
    expect(f.tokenName).toBe("tok-a");
    const g = withFilter(f, "tokenName", "");
    expect("tokenName" in g).toBe(false);
  });
});

describe("log pagination helpers", () => {
  it("nextPage advances by the page limit", () => {
    expect(nextPage({ limit: 50 }).offset).toBe(50);
    expect(nextPage({ limit: 50, offset: 50 }).offset).toBe(100);
  });

  it("prevPage steps back but never below 0", () => {
    expect(prevPage({ limit: 50, offset: 100 }).offset).toBe(50);
    expect(prevPage({ limit: 50, offset: 25 }).offset).toBe(0);
    expect(prevPage({ limit: 50 }).offset).toBe(0);
  });

  it("clampOffset floors negative offsets", () => {
    expect(clampOffset({ limit: 50, offset: -5 }).offset).toBe(0);
    expect(clampOffset({ limit: 50, offset: 10 })).toEqual({ limit: 50, offset: 10 });
  });

  it("resetToFirstPage drops a nonzero offset but keeps filters", () => {
    expect(resetToFirstPage({ limit: 50, offset: 100, service: "tavily" })).toEqual({
      limit: 50,
      offset: 0,
      service: "tavily",
    });
    expect(resetToFirstPage({ limit: 50, service: "tavily" })).toEqual({
      limit: 50,
      service: "tavily",
    });
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
