import { afterEach, describe, expect, it, vi } from "vitest";
import type { QueryClient } from "@tanstack/react-query";

import { qk } from "@/lib/query-keys";

import { invalidateKeysAndStats, syncCreditsRequest } from "./queries";
import type { SyncReport } from "./types";

function stubSync(report: SyncReport) {
  vi.stubGlobal(
    "fetch",
    vi.fn(
      async () =>
        ({
          ok: true,
          status: 200,
          text: async () => JSON.stringify(report),
        }) as unknown as Response,
    ),
  );
}

describe("invalidateKeysAndStats (key mutations refresh the stats summary)", () => {
  it("invalidates the keys list AND the stats summary — including the toggle path", async () => {
    const invalidateQueries = vi.fn(async () => {});
    const qc = { invalidateQueries } as unknown as QueryClient;
    await invalidateKeysAndStats(qc);
    // Toggling a key changes activeApiKeys on /api/stats, so both prefixes
    // must be refreshed (regression guard for FU16).
    expect(invalidateQueries).toHaveBeenCalledWith({ queryKey: qk.keys.all });
    expect(invalidateQueries).toHaveBeenCalledWith({ queryKey: qk.stats.all });
  });
});

describe("syncCreditsRequest (a capped pass is never reported as complete)", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("reports skipped keys in the clean-success notice", async () => {
    stubSync({ synced: 10, errors: 0, skipped: 4, results: [] });
    expect(await syncCreditsRequest()).toBe(
      "Credit sync: synced=10, errors=0, skipped=4 (next pass)",
    );
  });

  it("keeps the old success string when nothing was skipped", async () => {
    stubSync({ synced: 2, errors: 0, skipped: 0, results: [] });
    expect(await syncCreditsRequest()).toBe("Credit sync: synced=2, errors=0");
  });

  it("reports skipped in the partial-sync error too", async () => {
    stubSync({
      synced: 1,
      errors: 1,
      skipped: 7,
      results: [{ id: 5, ok: false, error: "429" }],
    });
    await expect(syncCreditsRequest()).rejects.toThrow(/skipped=7 \(next pass\)/);
  });
});
