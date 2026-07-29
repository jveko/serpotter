import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { adminFetch } from "@/lib/api";
import { qk } from "@/lib/query-keys";

import { settingsQueryOptions } from "./queries";
import type { SettingsDto } from "./types";

/**
 * Settings panel: drafts socialEnabled locally from query data;
 * saves via PUT /api/settings.
 */
export function SettingsPanel() {
  const qc = useQueryClient();
  const { data, error, isPending, isFetching } = useQuery(settingsQueryOptions);
  const [socialEnabled, setSocialEnabled] = useState(false);

  useEffect(() => {
    if (data) {
      setSocialEnabled(Boolean(data.socialEnabled));
    }
  }, [data]);

  const saveMutation = useMutation({
    mutationFn: (body: { socialEnabled: boolean }) =>
      adminFetch<SettingsDto>("/api/settings", {
        method: "PUT",
        body: JSON.stringify({ socialEnabled: body.socialEnabled }),
      }),
    meta: { successMessage: "Settings saved" },
    onSuccess: (out) => {
      qc.setQueryData(qk.settings.root(), out);
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

  let meta = "live";
  if (isPending) meta = "loading";
  else if (error && !data) meta = "error";
  else if (saveMutation.isPending) meta = "saving";
  else if (isFetching) meta = "refreshing";

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!data) return;
    saveMutation.mutate({ socialEnabled });
  }

  return (
    <section className="panel" id="settings">
      <div className="panel__head">
        <h2 className="panel__title">Settings</h2>
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
          </div>
        ) : data ? (
          <form onSubmit={handleSubmit} className="row">
            {errMsg && saveMutation.isError ? (
              <p className="banner__text err" role="alert">
                {errMsg}
              </p>
            ) : null}
            <label className="check">
              <input
                type="checkbox"
                checked={socialEnabled}
                onChange={(e) => setSocialEnabled(e.target.checked)}
                disabled={saveMutation.isPending}
              />
              socialEnabled (research social leg)
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
        ) : (
          <p className="empty">No settings</p>
        )}
      </div>
    </section>
  );
}
