import { afterEach, describe, expect, it, vi } from "vitest";
import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen } from "@testing-library/react";
import { createAppQueryClient } from "@/lib/query-client";
import { qk } from "@/lib/query-keys";

import { SettingsPanel } from "./SettingsPanel";
import type { AdminSessionDto, SettingsDto } from "./types";

const { navigate, invalidate, routerMock } = vi.hoisted(() => {
  const navigate = vi.fn();
  const invalidate = vi.fn();
  return { navigate, invalidate, routerMock: { navigate, invalidate } };
});

vi.mock("@/router", () => ({ router: routerMock }));

const SESSIONS: AdminSessionDto[] = [
  {
    token: "tok-current",
    tokenPreview: "tok…rent",
    userId: 1,
    expiresAt: "2099-01-01 00:00:00",
    createdAt: "2026-09-01 10:00:00",
    current: true,
  },
  {
    token: "tok-other",
    tokenPreview: "tok…ther",
    userId: 1,
    expiresAt: "2099-01-01 00:00:00",
    createdAt: "2026-09-02 10:00:00",
    current: false,
  },
];

/** Seeds the query cache and stubs the two fetches SettingsPanel makes. */
function mount(onUnauthorized: () => void) {
  const qc = createAppQueryClient({ onUnauthorized });
  qc.setQueryData<SettingsDto>(qk.settings.root(), { socialEnabled: false });
  qc.setQueryData<AdminSessionDto[]>(qk.admin.sessions(), SESSIONS);

  const fetchMock = vi.fn(async (url: string) => {
    if (url.includes("/api/admin/sessions")) {
      return new Response(JSON.stringify(SESSIONS), { status: 200 });
    }
    if (url.includes("/api/admin/change-password")) {
      // The server's RFC 9457 body: problemMessage() reads `detail`.
      return new Response(
        JSON.stringify({
          type: "https://serpotter.dev/errors/AuthenticationError",
          title: "Authentication Error",
          status: 401,
          detail: "Invalid current password",
        }),
        { status: 401, statusText: "Unauthorized" },
      );
    }
    return new Response(JSON.stringify({ socialEnabled: false }), { status: 200 });
  });
  vi.stubGlobal("fetch", fetchMock);

  const view = render(
    <QueryClientProvider client={qc}>
      <SettingsPanel />
    </QueryClientProvider>,
  );
  return { qc, view, fetchMock };
}

afterEach(() => {
  // test.globals is off, so RTL's auto-cleanup never registers. vi.unstubAllGlobals
  // is NOT covered by restoreAllMocks — without it the 401 fetch stub leaks
  // into every later test in this file.
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  navigate.mockClear();
  invalidate.mockClear();
});

describe("change-password 401 is a domain error, not a dead session", () => {
  it("renders the server message inline and keeps the session", async () => {
    const onUnauthorized = vi.fn();
    const { view } = mount(onUnauthorized);
    try {
      const current = screen.getByLabelText("Current password");
      const next = screen.getByLabelText("New password");
      await act(async () => {
        // React 19: set the value through the native setter so onChange fires.
        setValue(current, "wrong-password");
        setValue(next, "brand-new-password");
      });
      await act(async () => {
        screen.getByRole("button", { name: "Change password" }).click();
      });

      const alert = await screen.findByText("Invalid current password");
      expect(alert).toBeTruthy();
      // The admin is still in: no logout for a typo.
      expect(onUnauthorized).not.toHaveBeenCalled();
      expect(navigate).not.toHaveBeenCalled();
    } finally {
      view.unmount();
    }
  });
});

describe("session revoke guard", () => {
  it("disables Revoke on the current session row and explains why", () => {
    const { view } = mount(vi.fn());
    try {
      const buttons = screen.getAllByRole("button", { name: "Revoke" });
      expect(buttons).toHaveLength(2);
      const [currentBtn, otherBtn] = buttons as HTMLButtonElement[];
      expect(currentBtn.disabled).toBe(true);
      expect(currentBtn.title).toContain("current session");
      expect(otherBtn.disabled).toBe(false);
    } finally {
      view.unmount();
    }
  });
});

/** Fire React's onChange for a controlled input. */
function setValue(el: Element, value: string) {
  const proto = Object.getPrototypeOf(el) as { value: string };
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  const input = el as HTMLInputElement;
  setter?.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}
