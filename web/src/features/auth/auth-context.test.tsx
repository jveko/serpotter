import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render } from "@testing-library/react";

import { SECRET_KEY, SESSION_EXPIRES_KEY, SESSION_KEY } from "@/lib/constants";

import { AuthProvider, useAuth } from "./auth-context";
import { syncAuthSnapshotFromStorage } from "./auth-snapshot";

const { navigate, invalidate, routerMock } = vi.hoisted(() => {
  const navigate = vi.fn();
  const invalidate = vi.fn();
  return { navigate, invalidate, routerMock: { navigate, invalidate } };
});

vi.mock("@/router", () => ({ router: routerMock }));

/** Renders the live context so assertions read real state, not internals. */
function Probe() {
  const auth = useAuth();
  return <span data-testid="auth">{auth.isAuthenticated ? `auth:${auth.token}` : "anon"}</span>;
}

function mount() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  qc.setQueryData(["secret-admin-data"], { leak: true });
  const view = render(
    <QueryClientProvider client={qc}>
      <AuthProvider>
        <Probe />
      </AuthProvider>
    </QueryClientProvider>,
  );
  return { qc, view };
}

/** jsdom does not synthesize cross-tab storage events; deliver them. */
function deliverStorageEvent(key: string, oldValue: string | null, newValue: string | null) {
  act(() => {
    window.dispatchEvent(
      new StorageEvent("storage", { key, oldValue, newValue, storageArea: localStorage }),
    );
  });
}

beforeEach(() => {
  localStorage.clear();
  navigate.mockClear();
  invalidate.mockClear();
  syncAuthSnapshotFromStorage();
});

afterEach(() => {
  // test.globals is off, so RTL's auto-cleanup never registers. Without this
  // an unmounted AuthProvider stays subscribed to `storage` and mock counts
  // bleed across cases — the "login does NOT tear down" assertion would then
  // pass or fail for the wrong reason.
  cleanup();
  vi.restoreAllMocks();
});

describe("cross-tab auth teardown (AuthProvider)", () => {
  it("tears the tab down when another tab logs out", () => {
    localStorage.setItem(SESSION_KEY, "adm-tok");
    localStorage.setItem(SESSION_EXPIRES_KEY, "2099-01-01 00:00:00");
    syncAuthSnapshotFromStorage();

    const { qc, view } = mount();
    try {
      expect(view.getByTestId("auth").textContent).toBe("auth:adm-tok");

      localStorage.removeItem(SESSION_KEY);
      localStorage.removeItem(SESSION_EXPIRES_KEY);
      deliverStorageEvent(SESSION_KEY, "adm-tok", null);

      // Cached admin data is gone (not left on screen until this tab's own 401)
      // and the router went to /login.
      expect(qc.getQueryData(["secret-admin-data"])).toBeUndefined();
      expect(navigate).toHaveBeenCalledWith(expect.objectContaining({ to: "/login" }));
      expect(view.getByTestId("auth").textContent).toBe("anon");
    } finally {
      view.unmount();
    }
  });

  it("does NOT tear down when another tab logs in", () => {
    localStorage.setItem(SECRET_KEY, "secret-value");
    syncAuthSnapshotFromStorage();

    const { qc, view } = mount();
    try {
      deliverStorageEvent(SECRET_KEY, null, "secret-value");

      expect(navigate).not.toHaveBeenCalled();
      expect(qc.getQueryData(["secret-admin-data"])).toEqual({ leak: true });
      expect(view.getByTestId("auth").textContent).toBe("auth:secret-value");
    } finally {
      view.unmount();
    }
  });

  it("does NOT tear down on a session login in another tab", () => {
    localStorage.setItem(SESSION_KEY, "fresh-tok");
    localStorage.setItem(SESSION_EXPIRES_KEY, "2099-01-01 00:00:00");
    syncAuthSnapshotFromStorage();

    const { view } = mount();
    try {
      deliverStorageEvent(SESSION_KEY, "old-tok", "fresh-tok");
      expect(navigate).not.toHaveBeenCalled();
      expect(view.getByTestId("auth").textContent).toBe("auth:fresh-tok");
    } finally {
      view.unmount();
    }
  });

  it("does NOT tear down on an event whose token is present but already lapsed", () => {
    // A storage event re-reads the whole snapshot, and readStorage prefers
    // SESSION_KEY over SECRET_KEY. During another tab's secret switch the
    // first event can therefore pair a still-present session token with a
    // just-lapsed expiry: isAuthenticated is false even though the identity
    // is mid-switch, not dead. A global teardown here would destroy the fresh
    // login in every tab. The lapsed credential still ends locally.
    localStorage.setItem(SESSION_KEY, "stale-tok");
    localStorage.setItem(SESSION_EXPIRES_KEY, "2099-01-01 00:00:00");
    syncAuthSnapshotFromStorage();

    const { qc, view } = mount();
    try {
      localStorage.setItem(SECRET_KEY, "new-secret");
      localStorage.setItem(SESSION_EXPIRES_KEY, "2020-01-01 00:00:00");
      deliverStorageEvent(SECRET_KEY, null, "new-secret");

      // The identity is present, so no app-level teardown: no navigation, and
      // the tab keeps its cached data.
      expect(navigate).not.toHaveBeenCalled();
      expect(qc.getQueryData(["secret-admin-data"])).toEqual({ leak: true });
    } finally {
      view.unmount();
    }
  });
});
