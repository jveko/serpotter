import { afterEach, describe, expect, it, vi } from "vitest";
import { QueryClient } from "@tanstack/react-query";

import { toastManager } from "@/components/ui/toast";

import { HttpError } from "./api";
import { createAppQueryClient } from "./query-client";

afterEach(() => {
  vi.restoreAllMocks();
});

async function runMutation(
  qc: QueryClient,
  meta: Record<string, unknown>,
  err: Error,
): Promise<void> {
  const promise = qc
    .getMutationCache()
    .build(qc, { mutationFn: async () => Promise.reject(err), meta })
    .execute(undefined as never);
  await promise.catch(() => {});
}

describe("MutationCache 401 handling", () => {
  it("tears the session down by default", async () => {
    const onUnauthorized = vi.fn();
    const qc = createAppQueryClient({ onUnauthorized });
    await runMutation(qc, {}, new HttpError("session expired", 401));
    expect(onUnauthorized).toHaveBeenCalledTimes(1);
  });

  it("keeps the session when meta.authTeardown is false (domain 401)", async () => {
    const onUnauthorized = vi.fn();
    const qc = createAppQueryClient({ onUnauthorized });
    // change-password answers 401 authentication_error("Invalid current
    // password") AFTER require_admin passed: tearing down there logged the
    // admin out for a typo.
    await runMutation(qc, { authTeardown: false }, new HttpError("Invalid current password", 401));
    expect(onUnauthorized).not.toHaveBeenCalled();
  });

  it("does not swallow the error — the caller still sees the server message", async () => {
    const onUnauthorized = vi.fn();
    const qc = createAppQueryClient({ onUnauthorized });
    const mutation = qc.getMutationCache().build(qc, {
      mutationFn: async () => Promise.reject(new HttpError("Invalid current password", 401)),
      meta: { authTeardown: false },
    });
    const err = await mutation.execute(undefined as never).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(HttpError);
    expect((err as Error).message).toBe("Invalid current password");
    expect((err as HttpError).status).toBe(401);
  });

  it("surfaces a non-401 error as a toast carrying the server message", async () => {
    const onUnauthorized = vi.fn();
    const add = vi.spyOn(toastManager, "add").mockReturnValue("toast-1" as never);
    const qc = createAppQueryClient({ onUnauthorized });
    await runMutation(qc, {}, new HttpError("upstream exploded", 500));

    // Assert the toast actually fired with the message — a not-called
    // assertion alone would still pass if the error path never toasted.
    expect(add).toHaveBeenCalledWith(
      expect.objectContaining({ title: "upstream exploded", type: "error" }),
    );
    expect(onUnauthorized).not.toHaveBeenCalled();
  });

  it("prefers meta.errorMessage over the error's own text when provided", async () => {
    const add = vi.spyOn(toastManager, "add").mockReturnValue("toast-1" as never);
    const qc = createAppQueryClient({ onUnauthorized: vi.fn() });
    await runMutation(qc, { errorMessage: "Could not save" }, new HttpError("raw detail", 500));
    expect(add).toHaveBeenCalledWith(expect.objectContaining({ title: "Could not save" }));
  });

  it("stays silent for a 401 with authTeardown:false — the panel owns that message", async () => {
    const add = vi.spyOn(toastManager, "add").mockReturnValue("toast-1" as never);
    const qc = createAppQueryClient({ onUnauthorized: vi.fn() });
    await runMutation(qc, { authTeardown: false }, new HttpError("Invalid current password", 401));
    expect(add).not.toHaveBeenCalled();
  });
});
