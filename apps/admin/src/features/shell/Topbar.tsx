import { useQueryClient } from "@tanstack/react-query";
import { useRouterState } from "@tanstack/react-router";

import { SECTIONS, type SectionId } from "@/lib/constants";
import { qk } from "@/lib/query-keys";

import { usePanelStatus } from "./panel-status";

type TopbarProps = {
  onOpenCmdk: () => void;
};

/** pathname is basepath-stripped by the router (e.g. /stats); "" is the index. */
function activeSection(pathname: string): SectionId {
  const seg = pathname.replace(/^\//, "").split("/")[0] || "stats";
  const hit = SECTIONS.find((s) => s.id === seg);
  return hit ? hit.id : "stats";
}

function sectionLabel(id: SectionId): string {
  return SECTIONS.find((s) => s.id === id)?.label ?? "Stats";
}

function activePanelKey(id: SectionId): readonly unknown[] | null {
  switch (id) {
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
  }
}

/**
 * Page head: the section h1, the active panel's live status, and the actions.
 * Refresh invalidates only the active panel query prefix (playground has none).
 */
export function Topbar({ onOpenCmdk }: TopbarProps) {
  const qc = useQueryClient();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const status = usePanelStatus();

  const section = activeSection(pathname);
  const refreshKey = activePanelKey(section);

  function handleRefresh() {
    if (!refreshKey) return;
    void qc.invalidateQueries({ queryKey: refreshKey });
  }

  const isMac =
    typeof navigator !== "undefined" &&
    /Mac|iPhone|iPad|iPod/i.test(navigator.platform || navigator.userAgent);

  return (
    <header className="pagehead">
      <div className="pagehead__id">
        <h1 className="pagehead__title">{sectionLabel(section)}</h1>
        <p
          className={
            status.state === "error"
              ? "pagehead__status pagehead__status--error"
              : "pagehead__status"
          }
          aria-live="polite"
        >
          <b>{status.state}</b>
          {status.detail ? ` · ${status.detail}` : ""}
        </p>
      </div>
      <div className="pagehead__actions">
        <button type="button" className="btn btn--kbd btn--sm" onClick={onOpenCmdk}>
          Jump <kbd>{isMac ? "⌘" : "Ctrl"}K</kbd>
        </button>
        <button
          type="button"
          className="btn btn--secondary btn--sm"
          onClick={handleRefresh}
          disabled={refreshKey == null}
        >
          Refresh
        </button>
      </div>
    </header>
  );
}
