import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ConfirmDeleteDialog } from "@/components/ui/alert-dialog";
import { Dialog } from "@/components/ui/dialog";
import { usePublishPanelStatus } from "@/features/shell/panel-status";
import { qk } from "@/lib/query-keys";

import {
  createNodeRequest,
  deleteNodeRequest,
  nodesQueryOptions,
  testNodeRequest,
  toggleNodeRequest,
  updateNodeRequest,
} from "./queries";

import type { NodeRow, NodeTestResult } from "./types";

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
      // Node counter on the stats panel is aggregated from the same rows — keep it fresh.
      await Promise.all([
        qc.invalidateQueries({ queryKey: qk.nodes.all }),
        qc.invalidateQueries({ queryKey: qk.stats.all }),
      ]);
    },
  });

  const toggleMutation = useMutation({
    mutationFn: toggleNodeRequest,
    meta: { successMessage: "Node toggled" },
    onSuccess: async () => {
      await Promise.all([
        qc.invalidateQueries({ queryKey: qk.nodes.all }),
        qc.invalidateQueries({ queryKey: qk.stats.all }),
      ]);
    },
  });

  const deleteMutation = useMutation({
    mutationFn: deleteNodeRequest,
    meta: { successMessage: "Node deleted" },
    onSuccess: async () => {
      setDeleteId(null);
      await Promise.all([
        qc.invalidateQueries({ queryKey: qk.nodes.all }),
        qc.invalidateQueries({ queryKey: qk.stats.all }),
      ]);
    },
  });

  // Live connectivity probe (B12): one in-flight at a time; per-row results
  // render inline. Read-only — never invalidates nodes/stats.
  const [testResults, setTestResults] = useState<Record<number, NodeTestResult>>({});
  const testMutation = useMutation({
    mutationFn: (id: number) => testNodeRequest(id),
    onSuccess: (result, id) => {
      setTestResults((prev) => ({ ...prev, [id]: result }));
    },
    onError: (err, id) => {
      setTestResults((prev) => ({
        ...prev,
        [id]: { ok: false, error: err instanceof Error ? err.message : String(err) },
      }));
    },
  });

  const [editNode, setEditNode] = useState<NodeRow | null>(null);
  const [editHost, setEditHost] = useState("");
  const [editPort, setEditPort] = useState("8080");
  const [editProtocol, setEditProtocol] = useState("http");
  const [editUser, setEditUser] = useState("");
  const [editPass, setEditPass] = useState("");
  const [clearCreds, setClearCreds] = useState(false);

  const editMutation = useMutation({
    mutationFn: (p: {
      host?: string;
      port?: number;
      protocol?: string;
      username?: string | null;
      password?: string | null;
    }) => updateNodeRequest(editNode!.id, p),
    meta: { successMessage: "Node updated" },
    onSuccess: async () => {
      setEditNode(null);
      await Promise.all([
        qc.invalidateQueries({ queryKey: qk.nodes.all }),
        qc.invalidateQueries({ queryKey: qk.stats.all }),
      ]);
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

  const busy =
    createMutation.isPending ||
    toggleMutation.isPending ||
    deleteMutation.isPending ||
    editMutation.isPending ||
    testMutation.isPending;

  const testingId = testMutation.isPending ? (testMutation.variables ?? null) : null;

  function handleTest(id: number) {
    testMutation.mutate(id);
  }

  function mutMsg(err: unknown): string | null {
    if (!err) return null;
    return err instanceof Error ? err.message : String(err);
  }

  const mutErr =
    mutMsg(createMutation.error) ||
    mutMsg(toggleMutation.error) ||
    mutMsg(deleteMutation.error) ||
    mutMsg(editMutation.error);

  const loadErr = error instanceof Error ? error.message : error ? String(error) : null;

  const errMsg = mutErr || loadErr;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (createMutation.isPending) state = "creating";
  else if (toggleMutation.isPending) state = "toggling";
  else if (deleteMutation.isPending) state = "deleting";
  else if (editMutation.isPending) state = "editing";
  else if (testMutation.isPending) state = "testing";
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

  function openEdit(n: NodeRow) {
    setEditHost(n.host);
    setEditPort(String(n.port));
    setEditProtocol(n.protocol);
    setEditUser(n.username ?? "");
    setEditPass("");
    setClearCreds(false);
    setEditNode(n);
  }

  function submitEdit(e: React.FormEvent) {
    e.preventDefault();
    if (editNode == null) return;
    const body: {
      host?: string;
      port?: number;
      protocol?: string;
      username?: string | null;
      password?: string | null;
    } = {};
    const host = editHost.trim();
    if (host && host !== editNode.host) body.host = host;
    const port = Number(editPort);
    if (Number.isInteger(port) && port > 0 && port !== editNode.port) body.port = port;
    if (editProtocol !== editNode.protocol) body.protocol = editProtocol;
    if (clearCreds) {
      body.username = null;
      body.password = null;
    } else {
      if (editUser.trim() !== (editNode.username ?? "")) body.username = editUser.trim();
      if (editPass) body.password = editPass;
    }
    if (Object.keys(body).length === 0) return; // nothing changed
    editMutation.mutate(body);
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
            Per attempt: least-inflight enabled node, else direct (or 503 if
            REQUIRE_OUTBOUND_PROXY). xAI is always direct. Env OUTBOUND_PROXY is not used.
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
                <th>test</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {visible.length === 0 ? (
                <tr>
                  <td colSpan={12} className="empty">
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
                    <td>
                      {testResults[n.id] ? (
                        testResults[n.id].ok ? (
                          <span className="chip chip--ok">
                            {testResults[n.id].latencyMs != null
                              ? `${testResults[n.id].latencyMs} ms`
                              : "ok"}
                          </span>
                        ) : (
                          <span className="err" title={testResults[n.id].error ?? undefined}>
                            {testResults[n.id].error ?? "failed"}
                          </span>
                        )
                      ) : (
                        "—"
                      )}
                    </td>
                    <td className="table__actions">
                      <button
                        type="button"
                        className="btn btn--secondary btn--sm"
                        disabled={busy}
                        onClick={() => handleTest(n.id)}
                      >
                        {testingId === n.id ? "Testing…" : "Test"}
                      </button>
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
                        className="btn btn--secondary btn--sm"
                        disabled={busy}
                        onClick={() => openEdit(n)}
                      >
                        Edit
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

      <Dialog.Root
        open={editNode != null}
        onOpenChange={(open) => {
          if (!open && !editMutation.isPending) setEditNode(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Backdrop />
          <Dialog.Viewport>
            <Dialog.Popup aria-label="Edit node">
              <Dialog.Title>Edit node{editNode ? ` #${editNode.id}` : ""}</Dialog.Title>
              <Dialog.Description>
                Patch connection settings. The password is never read back — leave it empty to keep
                the current one. Changing username/password only takes effect on future attempts.
              </Dialog.Description>
              <form onSubmit={submitEdit} className="ui-dialog__form">
                <div className="row">
                  <label className="field">
                    <span className="field__label">Protocol</span>
                    <select
                      className="select"
                      value={editProtocol}
                      onChange={(e) => setEditProtocol(e.target.value)}
                      disabled={editMutation.isPending}
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
                      value={editHost}
                      onChange={(e) => setEditHost(e.target.value)}
                      disabled={editMutation.isPending}
                    />
                  </label>
                  <label className="field">
                    <span className="field__label">Port</span>
                    <input
                      className="input input--port"
                      value={editPort}
                      onChange={(e) => setEditPort(e.target.value)}
                      disabled={editMutation.isPending}
                    />
                  </label>
                </div>
                <div className="row">
                  <label className="field">
                    <span className="field__label">Username</span>
                    <input
                      className="input"
                      value={editUser}
                      onChange={(e) => setEditUser(e.target.value)}
                      disabled={editMutation.isPending || clearCreds}
                    />
                  </label>
                  <label className="field">
                    <span className="field__label">Password</span>
                    <input
                      className="input"
                      type="password"
                      value={editPass}
                      onChange={(e) => setEditPass(e.target.value)}
                      placeholder="new password — leave empty to keep"
                      disabled={editMutation.isPending || clearCreds}
                    />
                  </label>
                  <label className="check">
                    <input
                      type="checkbox"
                      checked={clearCreds}
                      onChange={(e) => setClearCreds(e.target.checked)}
                      disabled={editMutation.isPending}
                    />
                    Clear credentials
                  </label>
                </div>
                <div className="ui-alert__actions">
                  <Dialog.Close
                    className="btn btn--ghost btn--sm"
                    disabled={editMutation.isPending}
                  >
                    Cancel
                  </Dialog.Close>
                  <button
                    type="submit"
                    className="btn btn--primary btn--sm"
                    disabled={editMutation.isPending}
                    data-state={editMutation.isPending ? "loading" : undefined}
                  >
                    Save
                  </button>
                </div>
              </form>
            </Dialog.Popup>
          </Dialog.Viewport>
        </Dialog.Portal>
      </Dialog.Root>

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
