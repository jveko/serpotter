import { describe, expect, it } from "vitest";

import { passwordPolicyError, reconcileSocialDraft } from "./queries";

describe("reconcileSocialDraft (unsaved toggle survives refetch)", () => {
  it("keeps the dirty toggle across a refetch", () => {
    // User switched ON (current=true) but server still says off (saved=false):
    // a refetch must not clobber the unsaved value.
    expect(reconcileSocialDraft(true, false, true)).toBe(true);
  });

  it("adopts the server value on first load (untouched)", () => {
    expect(reconcileSocialDraft(false, true, false)).toBe(true);
  });

  it("adopts the server value when the draft matches it", () => {
    expect(reconcileSocialDraft(true, true, true)).toBe(true);
    expect(reconcileSocialDraft(false, false, true)).toBe(false);
  });
});

describe("passwordPolicyError (B14 change-password gate)", () => {
  it("rejects passwords shorter than 8 characters", () => {
    expect(passwordPolicyError("short")).toBe("New password must be at least 8 characters");
    expect(passwordPolicyError("       ")).toBe("New password must be at least 8 characters");
  });

  it("accepts 8+ character passwords", () => {
    expect(passwordPolicyError("longenough")).toBeNull();
    expect(passwordPolicyError("  eightch8  ")).toBeNull();
  });
});
