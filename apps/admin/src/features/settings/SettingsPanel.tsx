import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { usePublishPanelStatus } from "@/features/shell/panel-status";
import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import {
  adminSessionsQueryOptions,
  changePasswordRequest,
  passwordPolicyError,
  reconcileSocialDraft,
  revokeAdminSessionRequest,
  settingsQueryOptions,
} from "./queries";
import type { AdminSessionDto, SettingsDto } from "./types";

/**
 * Settings panel: drafts socialEnabled locally from query data;
 * saves via PUT /api/settings.
 */
export function SettingsPanel() {
  const qc = useQueryClient();
  const { data, error, isPending, isFetching } = useQuery(settingsQueryOptions);
  const [socialEnabled, setSocialEnabled] = useState(false);
  const [touched, setTouched] = useState(false);

  useEffect(() => {
    if (data) {
      // A refetch (Refresh button, window-focus refetch) must not clobber an
      // unsaved toggle — reconcileSocialDraft keeps the draft while dirty.
      setSocialEnabled((current) =>
        reconcileSocialDraft(current, Boolean(data.socialEnabled), touched),
      );
    }
  }, [data, touched]);

  const saveMutation = useMutation({
    mutationFn: (body: { socialEnabled: boolean }) =>
      adminFetch<SettingsDto>("/api/settings", {
        method: "PUT",
        body: JSON.stringify({ socialEnabled: body.socialEnabled }),
      }),
    meta: { successMessage: "Settings saved" },
    onSuccess: (out) => {
      qc.setQueryData(qk.settings.root(), out);
      setTouched(false);
    },
  });

  // B14: password rotation + session revocation.
  const [currentPassword, setCurrentPassword] = useState("");
  const [newPassword, setNewPassword] = useState("");
  const sessionsQuery = useQuery(adminSessionsQueryOptions);
  const changeMutation = useMutation({
    mutationFn: ({ current, next }: { current: string; next: string }) =>
      changePasswordRequest(current, next),
    onSuccess: () => {
      setCurrentPassword("");
      setNewPassword("");
      // Other sessions were revoked server-side; refresh the list.
      void qc.invalidateQueries({ queryKey: qk.admin.sessions() });
    },
  });
  const revokeMutation = useMutation({
    mutationFn: revokeAdminSessionRequest,
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: qk.admin.sessions() });
    },
  });

  const changeErr = changeMutation.error
    ? changeMutation.error instanceof Error
      ? changeMutation.error.message
      : String(changeMutation.error)
    : null;
  const policyErr = newPassword ? passwordPolicyError(newPassword) : null;
  const changeBlocked =
    !currentPassword || !newPassword || policyErr !== null || changeMutation.isPending;

  const sessions: AdminSessionDto[] = Array.isArray(sessionsQuery.data) ? sessionsQuery.data : [];
  const revokeErr = revokeMutation.error
    ? revokeMutation.error instanceof Error
      ? revokeMutation.error.message
      : String(revokeMutation.error)
    : null;

  const errMsg =
    saveMutation.error instanceof Error
      ? saveMutation.error.message
      : saveMutation.error
        ? String(saveMutation.error)
        : error instanceof Error
          ? error.message
          : error
            ? String(error)
            : null;

  let state = "live";
  if (isPending) state = "loading";
  else if (error && !data) state = "error";
  else if (saveMutation.isPending) state = "saving";
  else if (isFetching) state = "refreshing";

  const saved = data ? Boolean(data.socialEnabled) : false;
  const dirty = Boolean(data) && socialEnabled !== saved;
  usePublishPanelStatus(
    state,
    data ? `socialEnabled ${saved ? "on" : "off"}${dirty ? " · unsaved change" : ""}` : undefined,
  );

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!data) return;
    saveMutation.mutate({ socialEnabled });
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
      <p className="err" role="alert">
        {errMsg}
      </p>
    );
  }

  if (!data) return <p className="empty">No settings</p>;

  return (
    <section className="block" id="settings" aria-labelledby="settings-research">
      <div className="block__head">
        <h2 className="block__title" id="settings-research">
          Research
        </h2>
        <p className="block__note">
          Runtime flags for the research pipeline. Saved instance-wide via{" "}
          <span className="mono">PUT /api/settings</span>.
        </p>
      </div>
      {errMsg && saveMutation.isError ? (
        <p className="err" role="alert">
          {errMsg}
        </p>
      ) : null}
      <form onSubmit={handleSubmit} className="row">
        <label className="check">
          <input
            type="checkbox"
            checked={socialEnabled}
            onChange={(e) => {
              setTouched(true);
              setSocialEnabled(e.target.checked);
            }}
            disabled={saveMutation.isPending}
          />
          socialEnabled — run the social leg during research
        </label>
        <button
          type="submit"
          className="btn btn--primary btn--sm"
          disabled={saveMutation.isPending}
          data-state={saveMutation.isPending ? "loading" : undefined}
        >
          Save settings
        </button>
      </form>

      <div className="block__head">
        <h2 className="block__title">Admin security</h2>
        <p className="block__note">
          Rotate the admin password (revokes every other session) and manage active sessions via{" "}
          <span className="mono">/api/admin/sessions</span>.
        </p>
      </div>
      <form
        className="row"
        onSubmit={(e) => {
          e.preventDefault();
          if (!policyErr) {
            changeMutation.mutate({ current: currentPassword, next: newPassword });
          }
        }}
      >
        <label className="field">
          <span className="field__label">Current password</span>
          <input
            type="password"
            className="input"
            value={currentPassword}
            onChange={(e) => setCurrentPassword(e.target.value)}
            autoComplete="current-password"
          />
        </label>
        <label className="field">
          <span className="field__label">New password</span>
          <input
            type="password"
            className="input"
            value={newPassword}
            onChange={(e) => setNewPassword(e.target.value)}
            placeholder="min 8 characters"
            minLength={8}
            autoComplete="new-password"
          />
        </label>
        <button
          type="submit"
          className="btn btn--primary btn--sm"
          disabled={changeBlocked}
          data-state={changeMutation.isPending ? "loading" : undefined}
        >
          Change password
        </button>
      </form>
      {policyErr ? (
        <p className="err" role="alert">
          {policyErr}
        </p>
      ) : null}
      {changeErr ? (
        <p className="err" role="alert">
          {changeErr}
        </p>
      ) : null}
      {changeMutation.isSuccess ? (
        <p className="chip chip--ok" role="status">
          Password changed — other sessions were revoked.
        </p>
      ) : null}
      <div className="table-scroll bleed">
        <table className="table">
          <thead>
            <tr>
              <th>session</th>
              <th>createdAt</th>
              <th>expiresAt</th>
              <th>user</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {sessions.length === 0 ? (
              <tr>
                <td colSpan={5} className="empty">
                  {sessionsQuery.isPending && !sessionsQuery.data ? "Loading…" : "No sessions"}
                </td>
              </tr>
            ) : (
              sessions.map((s) => (
                <tr key={s.token}>
                  <td className="mono">
                    {s.tokenPreview}
                    {s.current ? <span className="chip">current</span> : null}
                  </td>
                  <td className="mono">{s.createdAt}</td>
                  <td className="mono">{s.expiresAt}</td>
                  <td>{s.userId}</td>
                  <td>
                    <button
                      type="button"
                      className="btn btn--secondary btn--sm"
                      disabled={revokeMutation.isPending}
                      onClick={() => revokeMutation.mutate(s.token)}
                    >
                      Revoke
                    </button>
                  </td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
      {revokeErr ? (
        <p className="err" role="alert">
          {revokeErr}
        </p>
      ) : null}
    </section>
  );
}
