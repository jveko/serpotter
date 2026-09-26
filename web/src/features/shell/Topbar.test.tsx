import { afterEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen } from "@testing-library/react";

import { qk } from "@/lib/query-keys";

import { Topbar } from "./Topbar";

const { pathnameState } = vi.hoisted(() => ({ pathnameState: { pathname: "/settings" } }));

vi.mock("@tanstack/react-router", () => ({
  useRouterState: (opts: { select: (s: { location: { pathname: string } }) => unknown }) =>
    opts.select({ location: pathnameState }),
}));

afterEach(() => {
  // test.globals is off, so RTL's auto-cleanup never registers. restoreAllMocks
  // unwraps the invalidateQueries spy from refreshOn().
  cleanup();
  vi.restoreAllMocks();
});

function refreshOn(pathname: string): { keys: unknown[] } {
  pathnameState.pathname = pathname;
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const spy = vi.spyOn(qc, "invalidateQueries").mockResolvedValue();
  const view = render(
    <QueryClientProvider client={qc}>
      <Topbar onOpenCmdk={vi.fn()} />
    </QueryClientProvider>,
  );
  act(() => {
    screen.getByRole("button", { name: "Refresh" }).click();
  });
  const keys = spy.mock.calls.map((c) => c[0]);
  view.unmount();
  return { keys };
}

describe("Topbar Refresh keys", () => {
  it("refreshes both the settings and the admin session list on /settings", () => {
    // The session table on /settings lives under ["admin","sessions"]; without
    // it the page-head Refresh never refetched the table.
    const { keys } = refreshOn("/settings");
    expect(keys).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ queryKey: qk.settings.all }),
        expect.objectContaining({ queryKey: qk.admin.sessions() }),
      ]),
    );
  });

  it("leaves other sections' key sets alone", () => {
    const { keys } = refreshOn("/stats");
    expect(keys).toEqual([expect.objectContaining({ queryKey: qk.stats.all })]);
  });
});
