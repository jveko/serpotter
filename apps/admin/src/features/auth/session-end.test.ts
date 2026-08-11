import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { PLAY_TOKEN_KEY, SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

import { getAuthSnapshot, onAuthStorageChanged, syncAuthSnapshotFromStorage } from "./auth-snapshot";
import { broadcastAuthCleared, clearAuthStorage } from "./session-end";

describe("clearAuthStorage", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("removes all admin identity keys and re-syncs the snapshot", () => {
    localStorage.setItem(SECRET_KEY, "secret");
    localStorage.setItem(SESSION_KEY, "adm-tok");
    localStorage.setItem(SESSION_EXPIRES_KEY, "2099-01-01 00:00:00");
    syncAuthSnapshotFromStorage();
    expect(getAuthSnapshot().isAuthenticated).toBe(true);

    clearAuthStorage();

    expect(localStorage.getItem(SECRET_KEY)).toBeNull();
    expect(localStorage.getItem(SESSION_KEY)).toBeNull();
    expect(localStorage.getItem(SESSION_EXPIRES_KEY)).toBeNull();
    expect(getAuthSnapshot().isAuthenticated).toBe(false);
  });

  it("never touches PLAY_TOKEN_KEY (active-token survive-logout is intentional)", () => {
    localStorage.setItem(PLAY_TOKEN_KEY, "tok-keep");
    clearAuthStorage();
    expect(localStorage.getItem(PLAY_TOKEN_KEY)).toBe("tok-keep");
  });
});

describe("broadcastAuthCleared", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("notifies same-tab listeners", () => {
    const listener = vi.fn();
    window.addEventListener("serpotter:auth-cleared", listener);
    try {
      broadcastAuthCleared();
      expect(listener).toHaveBeenCalledTimes(1);
    } finally {
      window.removeEventListener("serpotter:auth-cleared", listener);
    }
  });
});

describe("cross-tab logout propagation", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("clears the other tab's snapshot via the storage event after clearAuthStorage", () => {
    // This tab is authenticated.
    localStorage.setItem(SESSION_KEY, "adm-tok");
    localStorage.setItem(SESSION_EXPIRES_KEY, "2099-01-01 00:00:00");
    syncAuthSnapshotFromStorage();
    expect(getAuthSnapshot().isAuthenticated).toBe(true);

    const handler = vi.fn();
    const unsubscribe = onAuthStorageChanged(handler);
    try {
      // Tab A logs out (clearAuthStorage). jsdom does not synthesize the
      // cross-tab event, so deliver what tab B's browser would receive.
      clearAuthStorage();
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: SESSION_KEY,
          oldValue: "adm-tok",
          newValue: null,
          storageArea: localStorage,
        }),
      );
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: SESSION_EXPIRES_KEY,
          oldValue: "2099-01-01 00:00:00",
          newValue: null,
          storageArea: localStorage,
        }),
      );

      expect(getAuthSnapshot().isAuthenticated).toBe(false);
      expect(handler).toHaveBeenCalled();
    } finally {
      unsubscribe();
    }
  });
});
