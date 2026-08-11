import { describe, expect, it } from "vitest";

import { reconcileSocialDraft } from "./queries";

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