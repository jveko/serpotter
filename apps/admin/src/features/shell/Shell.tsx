import { useEffect, useState, type ReactNode } from "react";

import { Cmdk } from "./Cmdk";
import { Sidebar } from "./Sidebar";
import { Topbar } from "./Topbar";

type ShellProps = {
  children: ReactNode;
};

/**
 * Authenticated chrome: Topbar + Sidebar + main outlet + colophon + CmdK.
 */
export function Shell({ children }: ShellProps) {
  const [cmdkOpen, setCmdkOpen] = useState(false);

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
    <div className="shell">
      <Topbar onOpenCmdk={() => setCmdkOpen(true)} />
      <div className="shell__body">
        <Sidebar />
        <main className="shell__main">
          <div className="workbench">{children}</div>
          <footer className="colophon">
            <p>Serpotter admin · Cobalt instrument panel</p>
          </footer>
        </main>
      </div>
      <Cmdk open={cmdkOpen} onOpenChange={setCmdkOpen} />
    </div>
  );
}
