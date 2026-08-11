import { beforeEach, describe, expect, it, vi } from "vitest";
import type { QueryClient } from "@tanstack/react-query";

import { PLAY_TOKEN_KEY } from "@/lib/constants";
import { qk } from "@/lib/query-keys";

import { invalidateTokensAndStats, maybeClearPlayToken } from "./queries";

describe("maybeClearPlayToken (delete clears the persisted playground token)", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("clears PLAY_TOKEN_KEY when the deleted token matches the created one", () => {
    localStorage.setItem(PLAY_TOKEN_KEY, "tok-abc");
    const cleared = maybeClearPlayToken(7, { id: 7, token: "tok-abc" });
    expect(cleared).toBe(true);
    expect(localStorage.getItem(PLAY_TOKEN_KEY)).toBeNull();
  });

  it("keeps the key when a different token is deleted", () => {
    localStorage.setItem(PLAY_TOKEN_KEY, "tok-abc");
    const cleared = maybeClearPlayToken(8, { id: 7, token: "tok-abc" });
    expect(cleared).toBe(false);
    expect(localStorage.getItem(PLAY_TOKEN_KEY)).toBe("tok-abc");
  });

  it("keeps the key when the stored value differs (created but never used in playground)", () => {
    localStorage.clear();
    const cleared = maybeClearPlayToken(7, { id: 7, token: "tok-abc" });
    expect(cleared).toBe(false);
  });

  it("keeps the key when no raw token is known (older rows are preview-only)", () => {
    localStorage.setItem(PLAY_TOKEN_KEY, "tok-abc");
    const cleared = maybeClearPlayToken(7, null);
    expect(cleared).toBe(false);
    expect(localStorage.getItem(PLAY_TOKEN_KEY)).toBe("tok-abc");
  });
});

describe("invalidateTokensAndStats (create/delete refresh the stats summary)", () => {
  it("invalidates the tokens list AND the stats summary", async () => {
    const invalidateQueries = vi.fn(async () => {});
    const qc = { invalidateQueries } as unknown as QueryClient;
    await invalidateTokensAndStats(qc);
    expect(invalidateQueries).toHaveBeenCalledWith({ queryKey: qk.tokens.all });
    expect(invalidateQueries).toHaveBeenCalledWith({ queryKey: qk.stats.all });
  });
});