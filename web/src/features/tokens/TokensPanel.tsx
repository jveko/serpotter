import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";

import { ConfirmDeleteDialog } from "@/components/ui/alert-dialog";
import { Dialog } from "@/components/ui/dialog";
import { usePublishPanelStatus } from "@/features/shell/panel-status";
import { relativeTime } from "@/lib/relative-time";

import { forgetCapturedToken, rememberCapturedToken } from "./captured-tokens";
import {
  createTokenRequest,
  deleteTokenRequest,
  invalidateTokensAndStats,
  maybeClearPlayToken,
  tokensQueryOptions,
  useInPlayground,
} from "./queries";

/**
 * API tokens panel: list + create (dialog) + delete confirm.
 * Created tokens are remembered (session-scoped) for the playground picker.
 * "Use in playground" sets PLAY_TOKEN_KEY + event, then navigates to
 * /playground (playground reads the token on mount).
 */
export function TokensPanel() {
  const qc = useQueryClient();
  const navigate = useNavigate();
  const { data, error, isPending, isFetching, refetch } = useQuery(tokensQueryOptions);
  const [tokenName, setTokenName] = useState("admin");
  const [nameErr, setNameErr] = useState("");
  const [createOpen, setCreateOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  /** Raw value + id of the just-created token — the only one whose full plaintext the client ever holds. */
  const [createdToken, setCreatedToken] = useState<{
    id: number;
    name: string;
    token: string;
  } | null>(null);
  const [filter, setFilter] = useState("");
  const [deleteId, setDeleteId] = useState<number | null>(null);

  const createMutation = useMutation({
    mutationFn: createTokenRequest,
    meta: { successMessage: "Token created" },
    onSuccess: async (row) => {
      if (row.id != null && row.token) {
        rememberCapturedToken(row.id, row.name, row.token);
        setCreatedToken({ id: row.id, name: row.name, token: row.token });
      }
      await invalidateTokensAndStats(qc);
    },
  });

  const deleteMutation = useMutation({
    mutationFn: deleteTokenRequest,
    meta: { successMessage: "Token deleted" },
    onSuccess: async (_data, deletedId) => {
      setDeleteId(null);
      forgetCapturedToken(Number(deletedId));
      if (createdToken && String(deletedId) === String(createdToken.id)) {
        // The just-created token was deleted — retire its one-shot reveal and
        // drop the persisted playground token if it matches (revoked tokens
        // must not linger in localStorage).
        setCreatedToken(null);
      }
      maybeClearPlayToken(deletedId, createdToken);
      await invalidateTokensAndStats(qc);
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
    const name = tokenName.trim();
    if (!name) {
      setNameErr("Token name is required.");
      return;
    }
    setNameErr("");
    createMutation.mutate({ name });
  }

  function closeCreate() {
    if (createMutation.isPending) return;
    setCreateOpen(false);
    setCreatedToken(null);
    setCopied(false);
    setNameErr("");
  }

  async function copyToken() {
    if (!createdToken?.token) return;
    try {
      await navigator.clipboard.writeText(createdToken.token);
      setCopied(true);
    } catch {
      // Clipboard unavailable — the token stays visible in the read-only field.
    }
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
        <button
          type="button"
          className="btn btn--primary btn--sm"
          disabled={busy}
          onClick={() => {
            setCreatedToken(null);
            setCopied(false);
            setNameErr("");
            setTokenName("admin");
            setCreateOpen(true);
          }}
        >
          Create token
        </button>
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
                    <td className="mono">{t.createdAt ? relativeTime(t.createdAt) : "—"}</td>
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

      <Dialog.Root
        open={createOpen}
        onOpenChange={(open) => {
          if (!open) closeCreate();
        }}
      >
        <Dialog.Portal>
          <Dialog.Backdrop />
          <Dialog.Viewport>
            <Dialog.Popup aria-label="Create token">
              <Dialog.Title>Create token</Dialog.Title>
              <Dialog.Description>
                Client tokens (<span className="mono">tok-…</span>) authenticate the public API. The
                full value is shown once, at creation.
              </Dialog.Description>
              {createdToken ? (
                <div className="ui-dialog__form">
                  <label className="field">
                    <span className="field__label">Token (shown once)</span>
                    <input
                      className="input input--mono"
                      value={createdToken.token}
                      readOnly
                      onFocus={(e) => e.currentTarget.select()}
                    />
                  </label>
                  <div className="ui-alert__actions">
                    <button
                      type="button"
                      className="btn btn--secondary btn--sm"
                      disabled={busy}
                      onClick={() => void copyToken()}
                    >
                      {copied ? "Copied" : "Copy"}
                    </button>
                    <button
                      type="button"
                      className="btn btn--primary btn--sm"
                      disabled={busy}
                      onClick={() => {
                        useInPlayground(createdToken.token);
                        closeCreate();
                        void navigate({ to: "/playground" });
                      }}
                    >
                      Use in playground
                    </button>
                    <Dialog.Close className="btn btn--ghost btn--sm">Done</Dialog.Close>
                  </div>
                </div>
              ) : (
                <form onSubmit={handleCreate} className="ui-dialog__form">
                  {nameErr || mutErr ? (
                    <p className="err" role="alert">
                      {nameErr || mutErr}
                    </p>
                  ) : null}
                  <label className="field">
                    <span className="field__label">Name</span>
                    <input
                      className="input"
                      value={tokenName}
                      onChange={(e) => {
                        setTokenName(e.target.value);
                        setNameErr("");
                      }}
                      placeholder="name"
                      required
                      disabled={createMutation.isPending}
                    />
                  </label>
                  <div className="ui-alert__actions">
                    <Dialog.Close
                      className="btn btn--ghost btn--sm"
                      disabled={createMutation.isPending}
                    >
                      Cancel
                    </Dialog.Close>
                    <button
                      type="submit"
                      className="btn btn--primary btn--sm"
                      disabled={createMutation.isPending}
                      data-state={createMutation.isPending ? "loading" : undefined}
                    >
                      Create token
                    </button>
                  </div>
                </form>
              )}
            </Dialog.Popup>
          </Dialog.Viewport>
        </Dialog.Portal>
      </Dialog.Root>

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
