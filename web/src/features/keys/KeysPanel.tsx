import { useEffect, useMemo, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useSearch } from "@tanstack/react-router";

import { ConfirmDeleteDialog } from "@/components/ui/alert-dialog";
import { Dialog } from "@/components/ui/dialog";
import { usePublishPanelStatus } from "@/features/shell/panel-status";

import {
  createKeyRequest,
  deleteKeyRequest,
  invalidateKeysAndStats,
  keysQueryOptions,
  syncCreditsRequest,
  toggleKeyRequest,
  updateKeyRequest,
} from "./queries";

import type { KeyRow } from "./types";

/**
 * Provider keys panel: list, seed, toggle, delete, sync credits.
 * syncCredits honesty strings match useAdminData.js (partial → error, clean → notice).
 */
export function KeysPanel() {
  const qc = useQueryClient();
  const { focus } = useSearch({ from: "/_auth/keys" });
  const { data, error, isPending, isFetching, refetch } = useQuery(keysQueryOptions);
  const [keyService, setKeyService] = useState("tavily");
  const [keyValue, setKeyValue] = useState("");
  const [syncService, setSyncService] = useState("");
  const [filter, setFilter] = useState("");
  const [syncNotice, setSyncNotice] = useState("");
  const [deleteId, setDeleteId] = useState<number | null>(null);

  const createMutation = useMutation({
    mutationFn: createKeyRequest,
    meta: { successMessage: "Key created" },
    onSuccess: async () => {
      setKeyValue("");
      setSyncNotice("");
      await invalidateKeysAndStats(qc);
    },
  });

  const toggleMutation = useMutation({
    mutationFn: toggleKeyRequest,
    meta: { successMessage: "Key toggled" },
    onSuccess: async () => {
      setSyncNotice("");
      // Toggling changes activeApiKeys on /api/stats — refresh it alongside keys.
      await invalidateKeysAndStats(qc);
    },
  });

  const deleteMutation = useMutation({
    mutationFn: deleteKeyRequest,
    meta: { successMessage: "Key deleted" },
    onSuccess: async () => {
      setDeleteId(null);
      setSyncNotice("");
      await invalidateKeysAndStats(qc);
    },
  });

  const [editKey, setEditKey] = useState<KeyRow | null>(null);
  const [editService, setEditService] = useState("tavily");
  const [editKeyValue, setEditKeyValue] = useState("");

  const editMutation = useMutation({
    mutationFn: (p: { service?: string; key?: string }) => updateKeyRequest(editKey!.id, p),
    meta: { successMessage: "Key updated" },
    onSuccess: async () => {
      setEditKey(null);
      setSyncNotice("");
      await invalidateKeysAndStats(qc);
    },
  });

  const syncMutation = useMutation({
    mutationFn: syncCreditsRequest,
    meta: { silent: true },
    onSuccess: (msg) => {
      setSyncNotice(msg);
    },
    onError: () => {
      setSyncNotice("");
    },
    onSettled: async () => {
      // Partial sync still updates some keys server-side — always refresh.
      // Credits byService on the stats panel is aggregated from the same rows.
      await invalidateKeysAndStats(qc);
    },
  });

  const keys = Array.isArray(data) ? data : [];
  const keysLoaded = data != null;
  const q = filter.trim().toLowerCase();
  const visible = useMemo(
    () =>
      q
        ? keys.filter(
            (k) =>
              String(k.id).includes(q) ||
              (k.service || "").toLowerCase().includes(q) ||
              (k.keyPreview || "").toLowerCase().includes(q),
          )
        : keys,
    [keys, q],
  );

  const busy =
    createMutation.isPending ||
    toggleMutation.isPending ||
    deleteMutation.isPending ||
    syncMutation.isPending ||
    editMutation.isPending;

  function mutMsg(err: unknown): string | null {
    if (!err) return null;
    return err instanceof Error ? err.message : String(err);
  }

  const mutErr =
    mutMsg(createMutation.error) ||
    mutMsg(toggleMutation.error) ||
    mutMsg(deleteMutation.error) ||
    mutMsg(syncMutation.error) ||
    mutMsg(editMutation.error);

  const loadErr = error instanceof Error ? error.message : error ? String(error) : null;

  const errMsg = mutErr || loadErr;

  let state = "live";
  if (isPending && !data) state = "loading";
  else if (error && !data) state = "error";
  else if (createMutation.isPending) state = "creating";
  else if (toggleMutation.isPending) state = "toggling";
  else if (deleteMutation.isPending) state = "deleting";
  else if (syncMutation.isPending) state = "syncing";
  else if (editMutation.isPending) state = "editing";
  else if (isFetching) state = "refreshing";

  const activeCount = keys.filter((k) => k.active).length;
  usePublishPanelStatus(state, data ? `${keys.length} keys · ${activeCount} active` : undefined);

  useEffect(() => {
    if (focus == null || !keysLoaded) return;
    document.querySelector("[data-focus]")?.scrollIntoView({ block: "center" });
  }, [focus, keysLoaded]);

  function handleCreate(e: React.FormEvent) {
    e.preventDefault();
    if (!keyValue) return;
    // Match useAdminData setErr("") at start of createKey — drop sticky sync error.
    syncMutation.reset();
    createMutation.mutate({ service: keyService, key: keyValue });
  }

  function handleSync() {
    const payload: { service?: string } = {};
    if (syncService) payload.service = syncService;
    createMutation.reset();
    toggleMutation.reset();
    deleteMutation.reset();
    setSyncNotice("");
    syncMutation.mutate(payload);
  }

  function handleDelete(id: number) {
    syncMutation.reset();
    setDeleteId(id);
  }

  function handleToggle(id: number) {
    syncMutation.reset();
    toggleMutation.mutate(id);
  }

  function openEdit(k: KeyRow) {
    syncMutation.reset();
    setEditService(k.service);
    setEditKeyValue("");
    setEditKey(k);
  }

  function submitEdit(e: React.FormEvent) {
    e.preventDefault();
    if (editKey == null) return;
    const body: { service?: string; key?: string } = {};
    if (editService !== editKey.service) body.service = editService;
    if (editKeyValue.trim()) body.key = editKeyValue.trim();
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
      <section className="block" id="keys" aria-labelledby="keys-seed">
        <div className="block__head">
          <h2 className="block__title" id="keys-seed">
            Seed key
          </h2>
          <p className="block__note">
            Upstream provider credentials. Only a preview is ever read back.
          </p>
        </div>
        {mutErr ? (
          <p className="err" role="alert">
            {mutErr}
          </p>
        ) : null}
        <form onSubmit={handleCreate} className="row">
          <label className="field">
            <span className="field__label">Service</span>
            <select
              className="select"
              value={keyService}
              onChange={(e) => setKeyService(e.target.value)}
              disabled={busy}
            >
              <option value="tavily">tavily</option>
              <option value="firecrawl">firecrawl</option>
              <option value="exa">exa</option>
              <option value="xai">xai</option>
            </select>
          </label>
          <label className="field field--grow">
            <span className="field__label">API key</span>
            <input
              className="input input--mono"
              value={keyValue}
              onChange={(e) => setKeyValue(e.target.value)}
              placeholder="api key"
              disabled={busy}
            />
          </label>
          <button
            type="submit"
            className="btn btn--primary btn--sm"
            disabled={busy || !keyValue}
            data-state={createMutation.isPending ? "loading" : undefined}
          >
            Seed key
          </button>
        </form>
      </section>

      <section className="block" aria-labelledby="keys-sync">
        <div className="block__head">
          <h2 className="block__title" id="keys-sync">
            Credit sync
          </h2>
          <p className="block__note">
            Pulls remaining balances from the provider. exa and xai report soft errors.
          </p>
        </div>
        {syncNotice && !mutErr ? (
          <div className="banner" role="status">
            <p className="banner__text">{syncNotice}</p>
          </div>
        ) : null}
        <div className="row">
          <label className="field">
            <span className="field__label">Sync service</span>
            <select
              className="select"
              value={syncService}
              onChange={(e) => setSyncService(e.target.value)}
              disabled={busy}
            >
              <option value="">all (tavily+firecrawl)</option>
              <option value="tavily">tavily</option>
              <option value="firecrawl">firecrawl</option>
              <option value="exa">exa (soft-error)</option>
              <option value="xai">xai (soft-error)</option>
            </select>
          </label>
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            disabled={busy}
            data-state={syncMutation.isPending ? "loading" : undefined}
            onClick={handleSync}
          >
            Sync credits
          </button>
        </div>
      </section>

      <section className="block" aria-labelledby="keys-list">
        <div className="block__head">
          <h2 className="block__title" id="keys-list">
            Keys
          </h2>
        </div>
        <div className="row">
          <label className="field">
            <span className="field__label">Filter</span>
            <input
              className="input"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="id, service, preview"
            />
          </label>
        </div>
        <div className="table-scroll bleed">
          <table className="table">
            <thead>
              <tr>
                <th>id</th>
                <th>service</th>
                <th>preview</th>
                <th>active</th>
                <th>fails</th>
                <th>creditsRemaining</th>
                <th>creditsLimit</th>
                <th>usageSyncedAt</th>
                <th>inflight</th>
                <th>leaseUntil</th>
                <th>lastUsedAt</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {visible.length === 0 ? (
                <tr>
                  <td colSpan={14} className="empty">
                    No keys
                  </td>
                </tr>
              ) : (
                visible.map((k) => (
                  <tr key={k.id} data-focus={k.id === focus || undefined}>
                    <td className="num">{k.id}</td>
                    <td>{k.service}</td>
                    <td className="mono">{k.keyPreview}</td>
                    <td>{k.active ? "yes" : "no"}</td>
                    <td className="num">{k.consecutiveFails}</td>
                    <td className="mono num">{k.creditsRemaining ?? "—"}</td>
                    <td className="mono num">{k.creditsLimit ?? "—"}</td>
                    <td className="mono">{k.usageSyncedAt || "—"}</td>
                    <td className="num">{k.inflight ?? 0}</td>
                    <td className="mono">{k.leaseUntil || "—"}</td>
                    <td className="mono">{k.lastUsedAt || "—"}</td>
                    <td className="table__actions">
                      <button
                        type="button"
                        className="btn btn--secondary btn--sm"
                        disabled={busy}
                        onClick={() => handleToggle(k.id)}
                      >
                        {k.active ? "Disable" : "Enable"}
                      </button>
                      <button
                        type="button"
                        className="btn btn--secondary btn--sm"
                        disabled={busy}
                        onClick={() => openEdit(k)}
                      >
                        Edit
                      </button>
                      <button
                        type="button"
                        className="btn btn--danger btn--sm"
                        disabled={busy}
                        onClick={() => handleDelete(k.id)}
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
        open={editKey != null}
        onOpenChange={(open) => {
          if (!open && !editMutation.isPending) setEditKey(null);
        }}
      >
        <Dialog.Portal>
          <Dialog.Backdrop />
          <Dialog.Viewport>
            <Dialog.Popup aria-label="Edit key">
              <Dialog.Title>Edit key{editKey ? ` #${editKey.id}` : ""}</Dialog.Title>
              <Dialog.Description>
                Rotate the secret or change the service. The raw key is never shown; leave the field
                empty to keep the current secret.
              </Dialog.Description>
              <form onSubmit={submitEdit} className="ui-dialog__form">
                <label className="field">
                  <span className="field__label">Service</span>
                  <select
                    className="select"
                    value={editService}
                    onChange={(e) => setEditService(e.target.value)}
                    disabled={editMutation.isPending}
                  >
                    <option value="tavily">tavily</option>
                    <option value="firecrawl">firecrawl</option>
                    <option value="exa">exa</option>
                    <option value="xai">xai</option>
                  </select>
                </label>
                <label className="field field--grow">
                  <span className="field__label">API key</span>
                  <input
                    className="input input--mono"
                    value={editKeyValue}
                    onChange={(e) => setEditKeyValue(e.target.value)}
                    placeholder="new key — leave empty to keep"
                    disabled={editMutation.isPending}
                  />
                </label>
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
                    disabled={
                      editMutation.isPending ||
                      (editKey != null && editService === editKey.service && !editKeyValue.trim())
                    }
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
        title={deleteId != null ? `Delete key #${deleteId}?` : "Delete key"}
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
