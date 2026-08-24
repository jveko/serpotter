import { afterEach, describe, expect, it, vi } from "vitest";

import { secretProbeError, verifyAdminSecret } from "./api";

describe("secretProbeError (secret-mode login gate decision)", () => {
  it("accepts a successful probe", () => {
    expect(secretProbeError({ ok: true, status: 200 })).toBeNull();
  });

  it("surfaces 503 AdminDisabled as an error so the gate is NOT crossed", () => {
    const msg = secretProbeError({ ok: false, status: 503 });
    expect(msg).not.toBeNull();
    expect(msg).toMatch(/not configured/i);
  });

  it("surfaces 401 invalid secret", () => {
    expect(secretProbeError({ ok: false, status: 401 })).toMatch(/invalid/i);
  });

  it("maps a network failure to 'cannot reach'", () => {
    expect(secretProbeError({ ok: false, status: 0 })).toMatch(/cannot reach/i);
  });
});

describe("verifyAdminSecret", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns ok:false status 503 when ADMIN_SECRET is unset", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: false, status: 503 })),
    );
    const probe = await verifyAdminSecret("anything");
    expect(probe).toEqual({ ok: false, status: 503 });
  });

  it("returns ok:true on 200", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({ ok: true, status: 200 })),
    );
    const probe = await verifyAdminSecret("correct-secret");
    expect(probe).toEqual({ ok: true, status: 200 });
  });

  it("returns ok:false status 0 when the network fails", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("fetch failed");
      }),
    );
    const probe = await verifyAdminSecret("s");
    expect(probe).toEqual({ ok: false, status: 0 });
  });
});
