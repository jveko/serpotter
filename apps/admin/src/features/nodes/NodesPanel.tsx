import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ConfirmDeleteDialog } from "@/components/ui/alert-dialog";
import { qk } from "@/lib/query-keys";

import {
  createNodeRequest,
  deleteNodeRequest,
  nodesQueryOptions,
  toggleNodeRequest,
} from "./queries";

/**
 * Outbound nodes panel: list, create, toggle, delete.
 * Toggle labels: Disable when enabled, Enable when disabled.
 * Omits empty username on create (useAdminData parity). Clears password only on success.
 */
export function NodesPanel() {
  const qc = useQueryClient();
  const { data, error, isPending, isFetching, refetch } =
    useQuery(nodesQueryOptions);
  const [nodeHost, setNodeHost] = useState("127.0.0.1");
  const [nodePort, setNodePort] = useState("7890");
  const [nodeUser, setNodeUser] = useState("");
  const [nodePass, setNodePass] = useState("");
  const [filter, setFilter] = useState("");
  const [deleteId, setDeleteId] = useState<number | null>(null);

  const createMutation = useMutation({
    mutationFn: createNodeRequest,
    meta: { successMessage: "Node created" },
    onSuccess: async () => {
      setNodePass("");
      await qc.invalidateQueries({ queryKey: qk.nodes.all });
    },
  });

  const toggleMutation = useMutation({
    mutationFn: toggleNodeRequest,
    meta: { successMessage: "Node toggled" },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: qk.nodes.all });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: deleteNodeRequest,
    meta: { successMessage: "Node deleted" },
    onSuccess: async () => {
      setDeleteId(null);
      await qc.invalidateQueries({ queryKey: qk.nodes.all });
    },
  });

  const nodes = Array.isArray(data) ? data : [];
  const q = filter.trim().toLowerCase();
  const visible = q
    ? nodes.filter(
        (n) =>
          String(n.id).includes(q) ||
          (n.host || "").toLowerCase().includes(q) ||
          (n.username || "").toLowerCase().includes(q) ||
          (n.lastError || "").toLowerCase().includes(q) ||
          String(n.port).includes(q),
      )
    : nodes;

  const busy =
    createMutation.isPending ||
    toggleMutation.isPending ||
    deleteMutation.isPending;

  function mutMsg(err: unknown): string | null {
    if (!err) return null;
    return err instanceof Error ? err.message : String(err);
  }

  const mutErr =
    mutMsg(createMutation.error) ||
    mutMsg(toggleMutation.error) ||
    mutMsg(deleteMutation.error);

  const loadErr =
    error instanceof Error ? error.message : error ? String(error) : null;

  const errMsg = mutErr || loadErr;

  let meta = "live";
  if (isPending && !data) meta = "loading";
  else if (error && !data) meta = "error";
  else if (createMutation.isPending) meta = "creating";
  else if (toggleMutation.isPending) meta = "toggling";
  else if (deleteMutation.isPending) meta = "deleting";
  else if (isFetching) meta = "refreshing";

  function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!nodeHost.trim()) return;
    createMutation.mutate({
      host: nodeHost.trim(),
      port: nodePort,
      username: nodeUser,
      password: nodePass,
    });
  }

  function handleDelete(id: number) {
    setDeleteId(id);
  }

  function handleToggle(id: number) {
    toggleMutation.mutate(id);
  }

  return (
    <section className="panel" id="nodes">
      <div className="panel__head">
        <h2 className="panel__title">Outbound nodes</h2>
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
            <p className="panel__lede">
              Optional HTTP proxies for Tavily/Firecrawl/Exa. Fixed env
              OUTBOUND_PROXY (or HTTPS/HTTP_PROXY) process-stable else
              least-inflight enabled nodes per attempt else direct; xAI always
              direct.
            </p>
            {mutErr ? (
              <p className="banner__text err" role="alert">
                {mutErr}
              </p>
            ) : null}
            <form onSubmit={handleCreate} className="row">
              <label className="field">
                <span className="field__label">Host</span>
                <input
                  className="input"
                  value={nodeHost}
                  onChange={(e) => setNodeHost(e.target.value)}
                  placeholder="host"
                  required
                  disabled={busy}
                />
              </label>
              <label className="field">
                <span className="field__label">Port</span>
                <input
                  className="input input--port"
                  value={nodePort}
                  onChange={(e) => setNodePort(e.target.value)}
                  placeholder="port"
                  required
                  disabled={busy}
                />
              </label>
              <label className="field">
                <span className="field__label">Username</span>
                <input
                  className="input"
                  value={nodeUser}
                  onChange={(e) => setNodeUser(e.target.value)}
                  placeholder="username (opt)"
                  disabled={busy}
                />
              </label>
              <label className="field">
                <span className="field__label">Password</span>
                <input
                  className="input"
                  type="password"
                  value={nodePass}
                  onChange={(e) => setNodePass(e.target.value)}
                  placeholder="password (opt)"
                  disabled={busy}
                />
              </label>
              <button
                type="submit"
                className="btn btn--primary btn--sm"
                disabled={busy || !nodeHost.trim()}
                data-state={createMutation.isPending ? "loading" : undefined}
              >
                Add node
              </button>
            </form>
            <label className="field">
              <span className="field__label">Filter</span>
              <input
                className="input"
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                placeholder="id, host, user, error"
              />
            </label>
            <div className="table-wrap">
              <table className="table">
                <thead>
                  <tr>
                    <th>id</th>
                    <th>host</th>
                    <th>port</th>
                    <th>user</th>
                    <th>enabled</th>
                    <th>inflight</th>
                    <th>leaseUntil</th>
                    <th>consecutiveFails</th>
                    <th>lastError</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {visible.length === 0 ? (
                    <tr>
                      <td colSpan={10} className="empty">
                        No nodes
                      </td>
                    </tr>
                  ) : (
                    visible.map((n) => (
                      <tr key={n.id}>
                        <td>{n.id}</td>
                        <td className="mono">{n.host}</td>
                        <td>{n.port}</td>
                        <td className="mono">{n.username || "—"}</td>
                        <td>{n.enabled ? "yes" : "no"}</td>
                        <td>{n.inflight}</td>
                        <td className="mono">{n.leaseUntil || "—"}</td>
                        <td>{n.consecutiveFails ?? 0}</td>
                        <td
                          className="mono"
                          title={n.lastError || undefined}
                        >
                          {n.lastError || "—"}
                        </td>
                        <td className="table__actions">
                          <button
                            type="button"
                            className="btn btn--secondary btn--sm"
                            disabled={busy}
                            onClick={() => handleToggle(n.id)}
                          >
                            {n.enabled ? "Disable" : "Enable"}
                          </button>
                          <button
                            type="button"
                            className="btn btn--danger btn--sm"
                            disabled={busy}
                            onClick={() => handleDelete(n.id)}
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
      <ConfirmDeleteDialog
        open={deleteId != null}
        onOpenChange={(open) => {
          if (!open && !deleteMutation.isPending) setDeleteId(null);
        }}
        title={deleteId != null ? `Delete node #${deleteId}?` : "Delete node"}
        description="This cannot be undone."
        busy={deleteMutation.isPending}
        onConfirm={() => {
          if (deleteId == null) return;
          deleteMutation.mutate(deleteId);
        }}
      />
    </section>
  );
}
