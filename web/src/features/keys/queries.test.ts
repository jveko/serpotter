import { describe, expect, it, vi } from "vitest";
import type { QueryClient } from "@tanstack/react-query";

import { qk } from "@/lib/query-keys";

import { invalidateKeysAndStats } from "./queries";

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
