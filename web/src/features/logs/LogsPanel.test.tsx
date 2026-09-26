import { afterEach, describe, expect, it, vi } from "vitest";
import { QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { createAppQueryClient } from "@/lib/query-client";

import { LogsPanel } from "./LogsPanel";
import type { RequestLogRow } from "./types";

/** The required-field base every fixture below starts from. */
const BASE: RequestLogRow = {
  id: 1,
  createdAt: "2026-09-27T10:00:00Z",
  path: "/api/search",
  method: "GET",
  status: 200,
  cacheHit: false,
};

type LogsPanelProps = Parameters<typeof LogsPanel>[0];

/** Renders the real panel; the stub answers /api/request-logs with `rows`. */
function mount(rows: RequestLogRow[], props: LogsPanelProps = {}) {
  const qc = createAppQueryClient({ onUnauthorized: vi.fn() });
  const fetchMock = vi.fn(
    async (url: string) =>
      new Response(JSON.stringify(url.includes("/api/request-logs") ? rows : []), { status: 200 }),
  );
  vi.stubGlobal("fetch", fetchMock);
  const view = render(
    <QueryClientProvider client={qc}>
      <LogsPanel {...props} />
    </QueryClientProvider>,
  );
  return { view, fetchMock };
}

/** The request-logs URLs the panel actually asked the server for. */
function logRequests(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls
    .map((c) => String(c[0]))
    .filter((u) => u.includes("/api/request-logs"));
}

/** A GET /api/request-logs URL carries `key=value` as a query param. */
function param(url: string, key: string): string | null {
  return new URL(url, "http://panel.test").searchParams.get(key);
}

/**
 * The text a row renders under one column header. Going through the header
 * row means a column reordering breaks the test instead of silently shifting
 * every value one cell to the left.
 */
function cellUnder(header: string, rowMarker: string): string {
  const heads = Array.from(document.querySelectorAll("table thead th"));
  const col = heads.findIndex((th) => th.textContent === header);
  if (col < 0) throw new Error(`no column headed ${header}`);
  const row = Array.from(document.querySelectorAll("table tbody tr")).find((tr) =>
    tr.textContent?.includes(rowMarker),
  );
  if (!row) throw new Error(`no row containing ${rowMarker}`);
  return row.querySelectorAll("td")[col]?.textContent ?? "";
}

/** Fire React's onChange for a controlled input. */
function setValue(el: Element, value: string) {
  const proto = Object.getPrototypeOf(el) as { value: string };
  const setter = Object.getOwnPropertyDescriptor(proto, "value")?.set;
  const input = el as HTMLInputElement;
  setter?.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

afterEach(() => {
  // test.globals is off, so RTL's auto-cleanup never registers and the fetch
  // stub is not undone by restoreAllMocks.
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("usage columns (cost / tokens / cache)", () => {
  it("renders cost, the in/out/total triple, and a cache-hit chip for a row that has usage", async () => {
    const { view, fetchMock } = mount([
      {
        ...BASE,
        id: 7,
        path: "/api/ask",
        costEst: 0.0042,
        inputTokens: 10,
        outputTokens: 5,
        totalTokens: 15,
        cacheHit: true,
      },
    ]);
    try {
      await screen.findByText("/api/ask");
      // Fractional-cent cost keeps its 4th decimal: $0.00 would hide the spend.
      expect(cellUnder("costEst", "/api/ask")).toBe("$0.0042");
      expect(cellUnder("tokens (in/out/total)", "/api/ask")).toBe("10/5/15");
      expect(cellUnder("cacheHit", "/api/ask")).toBe("hit");
      expect(screen.getByText("hit").className).toContain("chip--ok");
      // The panel really did ask the server for these rows.
      expect(logRequests(fetchMock).length).toBeGreaterThan(0);
    } finally {
      view.unmount();
    }
  });

  it("degrades a row carrying no usage data to em dashes and a miss chip", async () => {
    const { view } = mount([{ ...BASE, id: 8, path: "/api/ping" }]);
    try {
      await screen.findByText("/api/ping");
      // Absent optionals must degrade, never print "undefined"/"null"/crash.
      expect(cellUnder("costEst", "/api/ping")).toBe("—");
      expect(cellUnder("tokens (in/out/total)", "/api/ping")).toBe("—");
      expect(cellUnder("cacheHit", "/api/ping")).toBe("miss");
      expect(screen.getByText("miss").className).not.toContain("chip--ok");
      expect(screen.queryByText("hit")).toBeNull();
    } finally {
      view.unmount();
    }
  });

  it("expands a row whose detail states the token split as one in/out/total pair", async () => {
    const { view } = mount([
      { ...BASE, id: 9, path: "/api/expand", inputTokens: 10, outputTokens: 5, totalTokens: 15 },
    ]);
    try {
      await screen.findByText("/api/expand");
      await act(async () => {
        screen.getByLabelText("Expand row details").click();
      });
      const detail = document.querySelector("dl.row-detail");
      expect(detail).not.toBeNull();
      const terms = Array.from(detail?.querySelectorAll("dt") ?? []).map((dt) => dt.textContent);
      // One pair for the triple, not one per token count.
      expect(terms.filter((t) => t?.startsWith("tokens"))).toEqual(["tokens (in/out/total)"]);
      const values = Array.from(detail?.querySelectorAll("dd") ?? []).map((dd) => dd.textContent);
      expect(values).toContain("10/5/15");
    } finally {
      view.unmount();
    }
  });
});

describe("errorKind filter", () => {
  it("reaches the server as an errorKind param, debounced into a single request", async () => {
    const { view, fetchMock } = mount([]);
    try {
      await screen.findByRole("table");
      const before = logRequests(fetchMock).length;
      // Fake timers: the 300ms quiet window is advanced explicitly, so the
      // test never races a real wall clock (advanceTimersByTimeAsync also
      // drains the microtasks the commit's fetch resolution needs).
      vi.useFakeTimers();
      try {
        const input = screen.getByLabelText("Error kind");
        // Seven keystrokes in one tick: the quiet window must collapse them,
        // so the server sees one extra GET — not seven, not one per key.
        for (const draft of ["T", "Ti", "Tim", "Time", "Timeo", "Timeou", "Timeout"]) {
          await act(async () => {
            setValue(input, draft);
          });
        }
        // The control echoes the draft immediately; the wire has not moved.
        expect((input as HTMLInputElement).value).toBe("Timeout");
        expect(logRequests(fetchMock)).toHaveLength(before);

        await act(async () => {
          await vi.advanceTimersByTimeAsync(300);
        });
        // The settled draft reached the server exactly once.
        expect(logRequests(fetchMock)).toHaveLength(before + 1);
        expect(param(logRequests(fetchMock)[before] ?? "", "errorKind")).toBe("Timeout");
      } finally {
        vi.useRealTimers();
      }
    } finally {
      view.unmount();
    }
  });

  it("seeds the request-logs URL from initialErrorKind", async () => {
    const { view, fetchMock } = mount([], { initialErrorKind: "Timeout" });
    try {
      await screen.findByRole("table");
      await waitFor(() => {
        expect(logRequests(fetchMock).map((u) => param(u, "errorKind"))).toContain("Timeout");
      });
      // The seeded deep link is visible in the control, not just on the wire.
      expect((screen.getByLabelText("Error kind") as HTMLInputElement).value).toBe("Timeout");
    } finally {
      view.unmount();
    }
  });
});
