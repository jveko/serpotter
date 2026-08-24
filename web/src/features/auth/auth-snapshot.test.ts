import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

import {
  getAuthSnapshot,
  onAuthStorageChanged,
  parseSessionExpiry,
  setAuthSnapshot,
  syncAuthSnapshotFromStorage,
} from "./auth-snapshot";

describe("parseSessionExpiry", () => {
  it("parses the backend space-separated UTC stamp as UTC, not local", () => {
    // Backend writes "YYYY-MM-DD HH:MM:SS" (SQLite datetime, no zone
    // designator). The fix must read it as UTC — equal to the explicit-Z ISO.
    const stamp = "2026-08-19 12:34:56";
    expect(parseSessionExpiry(stamp)).toBe(Date.parse("2026-08-19T12:34:56Z"));
  });

  it("passes ISO-8601 stamps through unchanged", () => {
    expect(parseSessionExpiry("2026-08-19T12:34:56Z")).toBe(Date.parse("2026-08-19T12:34:56Z"));
    expect(parseSessionExpiry("2026-08-19T12:34:56+07:00")).toBe(
      Date.parse("2026-08-19T12:34:56+07:00"),
    );
  });

  it("returns 0 for empty or unparseable values", () => {
    expect(parseSessionExpiry("")).toBe(0);
    expect(parseSessionExpiry(null)).toBe(0);
    expect(parseSessionExpiry(undefined)).toBe(0);
    expect(parseSessionExpiry("not-a-date")).toBe(0);
  });
});

describe("auth snapshot", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("tracks a session lapse at snapshot read time", () => {
    setAuthSnapshot("adm-tok", "2020-01-01 00:00:00");
    expect(getAuthSnapshot().isAuthenticated).toBe(false);
  });
});

describe("cross-tab auth sync (onAuthStorageChanged)", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("clears the snapshot when another tab logs out (storage event)", () => {
    // This tab is authenticated (keys written as applySessionToken would).
    localStorage.setItem(SESSION_KEY, "adm-tok");
    localStorage.setItem(SESSION_EXPIRES_KEY, "2099-01-01 00:00:00");
    syncAuthSnapshotFromStorage();
    expect(getAuthSnapshot().isAuthenticated).toBe(true);

    const handler = vi.fn();
    const unsubscribe = onAuthStorageChanged(handler);
    try {
      // Another tab's logout: clearAuthStorage removes the session key. jsdom
      // does not auto-fire cross-tab storage events, so deliver what the
      // browser would send to this tab.
      localStorage.removeItem(SESSION_KEY);
      localStorage.removeItem(SESSION_EXPIRES_KEY);
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

  it("adopts a login performed in another tab", () => {
    localStorage.setItem(SECRET_KEY, "secret-value");
    syncAuthSnapshotFromStorage();
    expect(getAuthSnapshot().isAuthenticated).toBe(true);

    const handler = vi.fn();
    const unsubscribe = onAuthStorageChanged(handler);
    try {
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: SECRET_KEY,
          oldValue: null,
          newValue: "secret-value",
          storageArea: localStorage,
        }),
      );
      expect(handler).toHaveBeenCalledWith(
        expect.objectContaining({ token: "secret-value", isAuthenticated: true }),
      );
    } finally {
      unsubscribe();
    }
  });

  it("ignores storage events for unrelated keys", () => {
    const handler = vi.fn();
    const unsubscribe = onAuthStorageChanged(handler);
    try {
      localStorage.setItem("some-other-key", "x");
      window.dispatchEvent(
        new StorageEvent("storage", {
          key: "some-other-key",
          newValue: "x",
          storageArea: localStorage,
        }),
      );
      expect(handler).not.toHaveBeenCalled();
    } finally {
      unsubscribe();
    }
  });
});
