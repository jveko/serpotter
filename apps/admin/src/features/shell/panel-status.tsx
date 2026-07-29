import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";

export type PanelStatus = {
  /** One machine word the page head prints: ready | loading | refreshing | error | creating … */
  state: string;
  /** Optional summary of real loaded data, e.g. "12 keys · 9 active". Never invented. */
  detail?: string;
};

const READY: PanelStatus = { state: "ready" };

type PanelStatusCtx = {
  status: PanelStatus;
  setStatus: (status: PanelStatus) => void;
};

const PanelStatusContext = createContext<PanelStatusCtx | null>(null);

/**
 * Holds the active panel's status so the page head can print it.
 * Wraps the page head and the outlet; panels publish, the head reads.
 */
export function PanelStatusProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<PanelStatus>(READY);
  const value = useMemo(() => ({ status, setStatus }), [status]);
  return <PanelStatusContext.Provider value={value}>{children}</PanelStatusContext.Provider>;
}

/** Page head reader. Returns "ready" outside a provider. */
export function usePanelStatus(): PanelStatus {
  return useContext(PanelStatusContext)?.status ?? READY;
}

/** Panels publish their live state up to the page head; resets on unmount. */
export function usePublishPanelStatus(state: string, detail?: string) {
  const setStatus = useContext(PanelStatusContext)?.setStatus;
  useEffect(() => {
    if (!setStatus) return;
    setStatus({ state, detail });
    return () => setStatus(READY);
  }, [setStatus, state, detail]);
}
