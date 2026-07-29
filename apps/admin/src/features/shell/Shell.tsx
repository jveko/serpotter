import { useEffect, useState, type ReactNode } from "react";
import { useRouterState } from "@tanstack/react-router";

import { Cmdk } from "./Cmdk";
import { PanelStatusProvider } from "./panel-status";
import { Sidebar } from "./Sidebar";
import { Topbar } from "./Topbar";

type ShellProps = {
  children: ReactNode;
};

/**
 * Rail Console chrome: graphite rail + page head + workspace + colophon + CmdK.
 * The view is keyed on pathname so each section arrives with one entrance beat.
 */
export function Shell({ children }: ShellProps) {
  const [cmdkOpen, setCmdkOpen] = useState(false);
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdkOpen((v) => !v);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="console">
      <Sidebar />
      <div className="main">
        <PanelStatusProvider>
          <Topbar onOpenCmdk={() => setCmdkOpen(true)} />
          <div className="view" key={pathname}>
            {children}
          </div>
        </PanelStatusProvider>
        <footer className="colophon">
          <p>Serpotter admin · session and ADMIN_SECRET auth · all times UTC</p>
        </footer>
      </div>
      <Cmdk open={cmdkOpen} onOpenChange={setCmdkOpen} />
    </div>
  );
}
