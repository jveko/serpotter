import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { SECTIONS } from "../constants.js";

/**
 * Command palette (⌘K) state and keyboard handling for section jump.
 * Global ⌘/Ctrl+K and Esc only when `enabled` (logged in).
 */
export function useCmdk(enabled) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const inputRef = useRef(null);

  const filteredSections = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return SECTIONS;
    return SECTIONS.filter(
      (s) =>
        s.label.toLowerCase().includes(q) || s.id.toLowerCase().includes(q),
    );
  }, [query]);

  const jumpTo = useCallback((id) => {
    setOpen(false);
    setQuery("");
    setIndex(0);
    document
      .getElementById(id)
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  useEffect(() => {
    if (!enabled) return;
    function onKey(e) {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setOpen((isOpen) => {
          setQuery("");
          setIndex(0);
          return !isOpen;
        });
        return;
      }
      if (!open) return;
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
        setQuery("");
        setIndex(0);
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled, open]);

  useEffect(() => {
    if (open && inputRef.current) {
      inputRef.current.focus();
    }
  }, [open]);

  useEffect(() => {
    setIndex(0);
  }, [query]);

  const onCmdkKeyDown = useCallback(
    (e) => {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setIndex((i) =>
          filteredSections.length
            ? Math.min(i + 1, filteredSections.length - 1)
            : 0,
        );
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setIndex((i) => Math.max(i - 1, 0));
      } else if (e.key === "Enter") {
        e.preventDefault();
        const sel = filteredSections[index];
        if (sel) jumpTo(sel.id);
      }
    },
    [filteredSections, index, jumpTo],
  );

  return {
    open,
    setOpen,
    query,
    setQuery,
    index,
    setIndex,
    filteredSections,
    jumpTo,
    onCmdkKeyDown,
    inputRef,
  };
}
