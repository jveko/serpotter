import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ConfirmDeleteDialog } from "@/components/ui/alert-dialog";
import { usePublishPanelStatus } from "@/features/shell/panel-status";
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
  const { data, error, isPending, isFetching, refetch } = useQuery(nodesQueryOptions);
  const [nodeHost, setNodeHost] = useState("127.0.0.1");
  const [nodeProtocol, setNodeProtocol] = useState("http");
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
          (n.protocol || "").toLowerCase().includes(q) ||
          (n.username || "").toLowerCase().includes(q) ||
          (n.lastError || "").toLowerCase().includes(q) ||
          String(n.port).includes(q),
      )
    : nodes;

  const busy = createMutation.isPending || toggleMutation.isPending || deleteMutation.isPending;

  function mutMsg(err: unknown): string | null {
    if (!err) return null;
    return err instanceof Error ? err.message : String(err);
  }

  const mutErr =
    mutMsg(createMutation.error) || mutMsg(toggleMutation.error) || mutMsg(deleteMutation.error);

  const loadErr = error instanceof Error ? error.message : error ? String(error) : null;

  const errMsg = mutErr || loadErr;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (createMutation.isPending) state = "creating";
  else if (toggleMutation.isPending) state = "toggling";
  else if (deleteMutation.isPending) state = "deleting";
  else if (isFetching) state = "refreshing";

  const enabledCount = nodes.filter((n) => n.enabled).length;
  usePublishPanelStatus(
    state,
    data ? `${nodes.length} nodes · ${enabledCount} enabled` : undefined,
  );

  function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!nodeHost.trim()) return;
    createMutation.mutate({
      host: nodeHost.trim(),
      port: nodePort,
      protocol: nodeProtocol,
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
      <section className="block" id="nodes" aria-labelledby="nodes-add">
        <div className="block__head">
          <h2 className="block__title" id="nodes-add">
            Add node
          </h2>
          <p className="block__note">
            HTTP, HTTPS, or SOCKS5 proxies for tavily, firecrawl, and exa. Username and password may
            be empty.
          </p>
        </div>
        {mutErr ? (
          <p className="err" role="alert">
            {mutErr}
          </p>
        ) : null}
        <form onSubmit={handleCreate} className="row">
          <label className="field">
            <span className="field__label">Protocol</span>
            <select
              className="input"
              value={nodeProtocol}
              onChange={(e) => setNodeProtocol(e.target.value)}
              disabled={busy}
            >
              <option value="http">HTTP</option>
              <option value="https">HTTPS</option>
              <option value="socks5">SOCKS5</option>
            </select>
          </label>
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
      </section>

      <section className="block" aria-labelledby="nodes-list">
        <div className="block__head">
          <h2 className="block__title" id="nodes-list">
            Nodes
          </h2>
          <p className="block__note">
            Per attempt: least-inflight enabled node, else direct (or 503 if REQUIRE_OUTBOUND_PROXY).
            xAI is always direct. Env OUTBOUND_PROXY is not used.
          </p>
        </div>
        <div className="row">
          <label className="field">
            <span className="field__label">Filter</span>
            <input
              className="input"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="id, host, protocol, user, error"
            />
          </label>
        </div>
        <div className="table-scroll bleed">
          <table className="table">
            <thead>
              <tr>
                <th>id</th>
                <th>protocol</th>
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
                  <td colSpan={11} className="empty">
                    No nodes
                  </td>
                </tr>
              ) : (
                visible.map((n) => (
                  <tr key={n.id}>
                    <td>{n.id}</td>
                    <td className="mono">{n.protocol}</td>
                    <td className="mono">{n.host}</td>
                    <td>{n.port}</td>
                    <td className="mono">{n.username || "—"}</td>
                    <td>{n.enabled ? "yes" : "no"}</td>
                    <td>{n.inflight}</td>
                    <td className="mono">{n.leaseUntil || "—"}</td>
                    <td>{n.consecutiveFails ?? 0}</td>
                    <td className="mono" title={n.lastError || undefined}>
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
      </section>

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
    </>
  );
}
