import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ConfirmDeleteDialog } from "@/components/ui/alert-dialog";
import { usePublishPanelStatus } from "@/features/shell/panel-status";
import { qk } from "@/lib/query-keys";

import {
  createTokenRequest,
  deleteTokenRequest,
  tokensQueryOptions,
  useInPlayground,
} from "./queries";

/**
 * API tokens panel: list + create (local newToken) + delete confirm.
 * useInPlayground only sets PLAY_TOKEN_KEY + event — no navigate.
 */
export function TokensPanel() {
  const qc = useQueryClient();
  const { data, error, isPending, isFetching, refetch } = useQuery(tokensQueryOptions);
  const [tokenName, setTokenName] = useState("admin");
  const [newToken, setNewToken] = useState("");
  const [filter, setFilter] = useState("");
  const [deleteId, setDeleteId] = useState<number | null>(null);

  const createMutation = useMutation({
    mutationFn: createTokenRequest,
    meta: { successMessage: "Token created" },
    onSuccess: async (row) => {
      setNewToken(row.token || "");
      await qc.invalidateQueries({ queryKey: qk.tokens.all });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: deleteTokenRequest,
    meta: { successMessage: "Token deleted" },
    onSuccess: async () => {
      setDeleteId(null);
      await qc.invalidateQueries({ queryKey: qk.tokens.all });
    },
  });

  const tokens = Array.isArray(data) ? data : [];
  const q = filter.trim().toLowerCase();
  const visible = q
    ? tokens.filter(
        (t) =>
          String(t.id).includes(q) ||
          (t.name || "").toLowerCase().includes(q) ||
          (t.tokenPreview || "").toLowerCase().includes(q),
      )
    : tokens;

  const busy = createMutation.isPending || deleteMutation.isPending;

  const mutErr =
    createMutation.error instanceof Error
      ? createMutation.error.message
      : createMutation.error
        ? String(createMutation.error)
        : deleteMutation.error instanceof Error
          ? deleteMutation.error.message
          : deleteMutation.error
            ? String(deleteMutation.error)
            : null;

  const loadErr = error instanceof Error ? error.message : error ? String(error) : null;

  const errMsg = mutErr || loadErr;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (busy) state = createMutation.isPending ? "creating" : "deleting";
  else if (isFetching) state = "refreshing";

  usePublishPanelStatus(
    state,
    data
      ? q
        ? `${visible.length} of ${tokens.length} tokens`
        : `${tokens.length} tokens`
      : undefined,
  );

  function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    createMutation.mutate({ name: tokenName });
  }

  if (isPending && !data) {
    return (
      <p className="empty" aria-busy="true">
        Loading…
      </p>
    );
  }

  if (error && !data) {
    return (
      <div className="block">
        <p className="err" role="alert">
          {errMsg}
        </p>
        <div className="row">
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            onClick={() => void refetch()}
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  return (
    <>
      <section className="block" id="tokens" aria-labelledby="tokens-create">
        <div className="block__head">
          <h2 className="block__title" id="tokens-create">
            Create token
          </h2>
          <p className="block__note">
            Client tokens (<span className="mono">tok-…</span>) authenticate the public API. The
            full value is shown once, at creation.
          </p>
        </div>
        {mutErr ? (
          <p className="err" role="alert">
            {mutErr}
          </p>
        ) : null}
        <form onSubmit={handleCreate} className="row">
          <label className="field">
            <span className="field__label">Name</span>
            <input
              className="input"
              value={tokenName}
              onChange={(e) => setTokenName(e.target.value)}
              placeholder="name"
              disabled={busy}
            />
          </label>
          <button
            type="submit"
            className="btn btn--primary btn--sm"
            disabled={busy}
            data-state={createMutation.isPending ? "loading" : undefined}
          >
            Create token
          </button>
        </form>
        {newToken ? (
          <div className="banner" role="status">
            <p className="banner__text">{newToken}</p>
            <button
              type="button"
              className="btn btn--secondary btn--sm"
              disabled={busy}
              onClick={() => useInPlayground(newToken)}
            >
              Use in playground
            </button>
          </div>
        ) : null}
      </section>

      <section className="block" aria-labelledby="tokens-list">
        <div className="block__head">
          <h2 className="block__title" id="tokens-list">
            Tokens
          </h2>
        </div>
        <div className="row">
          <label className="field">
            <span className="field__label">Filter</span>
            <input
              className="input"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="id, name, preview"
            />
          </label>
        </div>
        <div className="table-scroll bleed">
          <table className="table">
            <thead>
              <tr>
                <th>id</th>
                <th>name</th>
                <th>preview</th>
                <th>createdAt</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {visible.length === 0 ? (
                <tr>
                  <td colSpan={5} className="empty">
                    No tokens
                  </td>
                </tr>
              ) : (
                visible.map((t) => (
                  <tr key={t.id}>
                    <td>{t.id}</td>
                    <td>{t.name}</td>
                    <td className="mono">{t.tokenPreview}</td>
                    <td className="mono">{t.createdAt || "—"}</td>
                    <td className="table__actions">
                      <button
                        type="button"
                        className="btn btn--danger btn--sm"
                        disabled={busy}
                        onClick={() => setDeleteId(t.id)}
                      >
                        Delete
                      </button>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </section>

      <ConfirmDeleteDialog
        open={deleteId != null}
        onOpenChange={(open) => {
          if (!open && !deleteMutation.isPending) setDeleteId(null);
        }}
        title={deleteId != null ? `Delete token #${deleteId}?` : "Delete token"}
        description="This cannot be undone. Active clients using the token will fail."
        busy={deleteMutation.isPending}
        onConfirm={() => {
          if (deleteId == null) return;
          deleteMutation.mutate(deleteId);
        }}
      />
    </>
  );
}
