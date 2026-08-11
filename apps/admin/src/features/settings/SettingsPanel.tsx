import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { usePublishPanelStatus } from "@/features/shell/panel-status";
import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import { reconcileSocialDraft, settingsQueryOptions } from "./queries";
import type { SettingsDto } from "./types";

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
    </section>
  );
}
