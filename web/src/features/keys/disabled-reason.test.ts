import { describe, expect, it } from "vitest";

import { disabledReason } from "./disabled-reason";

describe("disabledReason (why a key is off, in the operator's words)", () => {
  it("shows no chip for an active key or a missing reason", () => {
    expect(disabledReason(null)).toBeNull();
    expect(disabledReason(undefined)).toBeNull();
    expect(disabledReason("")).toBeNull();
  });

  it("warns on vendor_suspended — a dead vendor account an operator must re-enable", () => {
    const chip = disabledReason("vendor_suspended");
    expect(chip?.modifier).toBe("warn");
    expect(chip?.label).toBe("vendor suspended");
    expect(chip?.title).toMatch(/operator/);
  });

  it("keeps manual neutral — an operator toggle, not a vendor failure", () => {
    const chip = disabledReason("manual");
    expect(chip?.modifier).toBe("");
    expect(chip?.label).toBe("manual");
  });

  it("keeps auth_fail neutral — the re-enable cron heals it, no operator needed", () => {
    const chip = disabledReason("auth_fail");
    expect(chip?.modifier).toBe("");
    expect(chip?.label).toBe("auth failure");
    expect(chip?.title).toMatch(/3/);
  });

  it("passes an unknown reason through verbatim rather than swallowing it", () => {
    const chip = disabledReason("billing_hold");
    expect(chip?.label).toBe("billing_hold");
    expect(chip?.title).toContain("billing_hold");
  });
});
