import { afterEach, describe, expect, it, vi } from "vitest";

import { PLAY_TOKEN_KEY } from "@/lib/constants";

import { buildPlaygroundRequest, runPlayground } from "./runPlayground";

describe("buildPlaygroundRequest", () => {
  it("builds a search request with defaults", () => {
    expect(buildPlaygroundRequest({ token: "", query: "rust axum" })).toEqual({
      path: "/api/search",
      body: { query: "rust axum", maxResults: 5 },
    });
  });

  it("builds an extract request", () => {
    expect(
      buildPlaygroundRequest({ token: "", mode: "extract", url: "https://example.com" }),
    ).toEqual({
      path: "/api/extract",
      body: { url: "https://example.com" },
    });
  });

  it("builds a research request with optional bounds", () => {
    expect(
      buildPlaygroundRequest({
        token: "",
        mode: "research",
        query: "q",
        maxResults: 3,
        scrapeTopN: "2",
      }),
    ).toEqual({ path: "/api/research", body: { query: "q", maxResults: 3, scrapeTopN: 2 } });
  });
});

describe("runPlayground", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    localStorage.clear();
  });

  function okFetch() {
    return {
      ok: true,
      status: 200,
      text: async () => "{}",
    } as unknown as Response;
  }

  it("persists the token on a successful search", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => okFetch()),
    );
    const out = await runPlayground({ token: "tok-abc", mode: "search", query: "q" });
    expect(out).toMatchObject({ ok: true, status: 200 });
    if (out.ok) expect(out.durationMs).toBeGreaterThanOrEqual(0);
    expect(localStorage.getItem(PLAY_TOKEN_KEY)).toBe("tok-abc");
  });

  it("does NOT flip a successful response to failure when localStorage throws", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => okFetch()),
    );
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new DOMException("QuotaExceededError", "QuotaExceededError");
    });
    const out = await runPlayground({ token: "tok-abc", mode: "search", query: "q" });
    expect(out).toMatchObject({ ok: true, status: 200 });
  });

  it("reports a non-2xx response as failure without persisting the token", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          ({
            ok: false,
            status: 429,
            statusText: "Too Many Requests",
            text: async () => "{}",
          }) as unknown as Response,
      ),
    );
    const out = await runPlayground({ token: "tok-abc", mode: "search", query: "q" });
    expect(out.ok).toBe(false);
    if (!out.ok) expect(out.status).toBe(429);
    expect(localStorage.getItem(PLAY_TOKEN_KEY)).toBeNull();
  });

  it("reports a network failure as status null", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("fetch failed");
      }),
    );
    const out = await runPlayground({ token: "tok-abc", mode: "search", query: "q" });
    expect(out).toMatchObject({ ok: false, status: null, error: "fetch failed" });
    if (!out.ok) expect(out.durationMs).toBeGreaterThanOrEqual(0);
  });
});
