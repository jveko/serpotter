import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

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
  const { data, error, isPending, isFetching, refetch } =
    useQuery(tokensQueryOptions);
  const [tokenName, setTokenName] = useState("admin");
  const [newToken, setNewToken] = useState("");
  const [filter, setFilter] = useState("");

  const createMutation = useMutation({
    mutationFn: createTokenRequest,
    onSuccess: async (row) => {
      setNewToken(row.token || "");
      await qc.invalidateQueries({ queryKey: qk.tokens.all });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: deleteTokenRequest,
    onSuccess: async () => {
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

  const loadErr =
    error instanceof Error ? error.message : error ? String(error) : null;

  const errMsg = mutErr || loadErr;

  let meta = "live";
  if (isPending && !data) meta = "loading";
  else if (error && !data) meta = "error";
  else if (busy) meta = createMutation.isPending ? "creating" : "deleting";
  else if (isFetching) meta = "refreshing";

  function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    createMutation.mutate({ name: tokenName });
  }

  function handleDelete(id: number) {
    if (!window.confirm(`Delete token #${id}?`)) return;
    deleteMutation.mutate(id);
  }

  return (
    <section className="panel" id="tokens">
      <div className="panel__head">
        <h2 className="panel__title">API tokens</h2>
        <span className="panel__meta">{meta}</span>
      </div>
      <div className="panel__body">
        {isPending && !data ? (
          <p className="empty" aria-busy="true">
            Loading…
          </p>
        ) : error && !data ? (
          <div className="banner" role="alert">
            <p className="banner__text err">{errMsg}</p>
            <button
              type="button"
              className="btn btn--secondary btn--sm"
              onClick={() => void refetch()}
            >
              Retry
            </button>
          </div>
        ) : (
          <>
            <form onSubmit={handleCreate} className="row">
              {mutErr ? (
                <p className="banner__text err" role="alert">
                  {mutErr}
                </p>
              ) : null}
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
              <>
                <p className="mono break">New token (copy once): {newToken}</p>
                <button
                  type="button"
                  className="btn btn--secondary btn--sm"
                  disabled={busy}
                  onClick={() => useInPlayground(newToken)}
                >
                  Use in playground
                </button>
              </>
            ) : null}
            <label className="field">
              <span className="field__label">Filter</span>
              <input
                className="input"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="id, name, preview"
              />
            </label>
            <div className="table-wrap">
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
                            className="btn btn--secondary btn--sm"
                            disabled={busy}
                            onClick={() => handleDelete(t.id)}
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
          </>
        )}
      </div>
    </section>
  );
}
