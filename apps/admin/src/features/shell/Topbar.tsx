import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRouter, useRouterState } from "@tanstack/react-router";

import { useAuth } from "@/features/auth/auth-context";
import { statsQueryOptions } from "@/features/stats/queries";
import type { SectionId } from "@/lib/constants";
import { qk } from "@/lib/query-keys";

type TopbarProps = {
  onOpenCmdk: () => void;
};

function activePanelKey(pathname: string): readonly unknown[] | null {
  // pathname is basepath-stripped by the router (e.g. /stats)
  const seg = pathname.replace(/^\//, "").split("/")[0] || "stats";
  switch (seg as SectionId | string) {
    case "stats":
      return qk.stats.all;
    case "settings":
      return qk.settings.all;
    case "tokens":
      return qk.tokens.all;
    case "keys":
      return qk.keys.all;
    case "nodes":
      return qk.nodes.all;
    case "logs":
      return qk.requestLogs.all;
    case "playground":
      return null;
    default:
      return null;
  }
}

/**
 * Cobalt top bar: wordmark, schema/exp chips, Jump/Refresh/Logout.
 * Refresh invalidates only the active panel query prefix.
 */
export function Topbar({ onOpenCmdk }: TopbarProps) {
  const auth = useAuth();
  const qc = useQueryClient();
  const router = useRouter();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const statsQ = useQuery({
    ...statsQueryOptions,
    // Optional chip — soft-fail; don't block shell
    retry: false,
  });
  const schemaVersion = statsQ.data?.schemaVersion;

  function handleRefresh() {
    const key = activePanelKey(pathname);
    if (!key) return;
    void qc.invalidateQueries({ queryKey: key });
  }

  function handleLogout() {
    // Explicit logout recipe (Task 4): do NOT call endAdminSession (would double-clear).
    auth.logout();
    qc.clear();
    void router.navigate({ to: "/login", search: { redirect: undefined } });
    void router.invalidate();
  }

  const isMac =
    typeof navigator !== "undefined" &&
    /Mac|iPhone|iPad|iPod/i.test(navigator.platform || navigator.userAgent);

  return (
    <header className="topbar">
      <div className="topbar__inner">
        <span className="wordmark">
          Serpotter<span className="wordmark__dot">.</span>
        </span>
        <div className="topbar__meta">
          {schemaVersion != null && (
            <span className="chip chip--live">
              <span className="chip__swatch" aria-hidden />
              schema {schemaVersion}
            </span>
          )}
          {auth.sessionExpiresAt ? (
            <span
              className="chip"
              title={`Admin session expires ${auth.sessionExpiresAt}`}
            >
              <span className="chip__swatch" aria-hidden />
              exp {auth.sessionExpiresAt}
            </span>
          ) : null}
        </div>
        <div className="topbar__actions">
          <button
            type="button"
            className="btn btn--kbd btn--sm"
            onClick={onOpenCmdk}
          >
            Jump <kbd>{isMac ? "⌘" : "Ctrl"}K</kbd>
          </button>
          <button
            type="button"
            className="btn btn--secondary btn--sm"
            onClick={handleRefresh}
            disabled={activePanelKey(pathname) == null}
          >
            Refresh
          </button>
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={handleLogout}
          >
            Logout
          </button>
        </div>
      </div>
    </header>
  );
}
