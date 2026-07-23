import React from "react";

/**
 * Admin shell topbar: brand, schema chip, jump/refresh/logout.
 */
export function Topbar({ stats, busy, onRefresh, onLogout, onOpenCmdk }) {
  return (
    <header className="topbar">
      <div className="topbar__inner">
        <span className="wordmark">
          Serpotter<span className="wordmark__dot">.</span>
        </span>
        <div className="topbar__meta">
          {stats?.schemaVersion != null && (
            <span className="chip chip--live">
              <span className="chip__swatch" aria-hidden />
              schema {stats.schemaVersion}
            </span>
          )}
          {busy && (
            <span className="chip chip--warn">
              <span className="chip__swatch" aria-hidden />
              busy
            </span>
          )}
        </div>
        <div className="topbar__actions">
          <button
            type="button"
            className="btn btn--kbd btn--sm"
            onClick={onOpenCmdk}
          >
            Jump <kbd>⌘K</kbd>
          </button>
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            disabled={busy}
            data-state={busy ? "loading" : undefined}
            onClick={onRefresh}
          >
            Refresh
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={onLogout}
          >
            Logout
          </button>
        </div>
      </div>
    </header>
  );
}
