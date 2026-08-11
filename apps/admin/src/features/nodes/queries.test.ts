import { afterEach, describe, expect, it, vi } from "vitest";

import { testNodeRequest } from "./queries";

describe("testNodeRequest (POST /api/nodes/{id}/test)", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("POSTs to the per-node test route and returns the latency", async () => {
    let captured: [string, RequestInit?] | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        captured = [url, init];
        return {
          ok: true,
          status: 200,
          text: async () => JSON.stringify({ ok: true, latencyMs: 123 }),
        } as unknown as Response;
      }),
    );

    const out = await testNodeRequest(7);
    expect(out).toEqual({ ok: true, latencyMs: 123 });

    expect(captured?.[0]).toBe("/api/nodes/7/test");
    expect(captured?.[1]?.method).toBe("POST");
  });

  it("passes ok:false + error through (probe failures are a 200 with ok:false)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          ({
            ok: true,
            status: 200,
            text: async () =>
              JSON.stringify({ ok: false, error: "connection failed: connection refused" }),
          }) as unknown as Response,
      ),
    );

    const out = await testNodeRequest(3);
    expect(out.ok).toBe(false);
    expect(out.error).toMatch(/refused/);
    expect(out.latencyMs).toBeUndefined();
  });

  it("throws HttpError on a non-2xx rejection (auth/network problem, not a probe result)", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          ({
            ok: false,
            status: 401,
            statusText: "Unauthorized",
            text: async () => JSON.stringify({ title: "Unauthorized" }),
          }) as unknown as Response,
      ),
    );

    await expect(testNodeRequest(1)).rejects.toMatchObject({ status: 401 });
  });
});