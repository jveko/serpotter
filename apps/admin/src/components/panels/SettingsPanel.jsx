import React, { useEffect, useState } from "react";

/**
 * Settings panel. Drafts socialEnabled locally from settings prop;
 * saves via onSave({ socialEnabled }) — no adminFetch.
 */
export function SettingsPanel({ settings, busy, onSave }) {
  const [socialEnabled, setSocialEnabled] = useState(false);

  useEffect(() => {
    if (settings) {
      setSocialEnabled(Boolean(settings.socialEnabled));
    }
  }, [settings]);

  function handleSubmit(e) {
    e.preventDefault();
    if (!settings) return;
    onSave({ socialEnabled });
  }

  return (
    <section className="panel" id="settings">
      <div className="panel__head">
        <h2 className="panel__title">Settings</h2>
      </div>
      <div className="panel__body">
        {settings ? (
          <form onSubmit={handleSubmit} className="row">
            <label className="check">
              <input
                type="checkbox"
                checked={socialEnabled}
                onChange={(e) => setSocialEnabled(e.target.checked)}
              />
              socialEnabled (research social leg)
            </label>
            <button
              type="submit"
              className="btn btn--primary btn--sm"
              disabled={busy}
              data-state={busy ? "loading" : undefined}
            >
              Save settings
            </button>
          </form>
        ) : (
          <p className="empty">Loading…</p>
        )}
      </div>
    </section>
  );
}
