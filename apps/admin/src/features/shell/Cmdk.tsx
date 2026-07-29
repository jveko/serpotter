import { Autocomplete } from "@base-ui/react/autocomplete";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useMemo, useState } from "react";

import { Dialog } from "@/components/ui/dialog";
import { SECTIONS, type SectionId } from "@/lib/constants";

type SectionItem = { id: SectionId; label: string; value: string };

type CmdkProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
};

/**
 * ⌘/Ctrl+K palette: Base UI Dialog + Autocomplete over SECTIONS.
 * Click or Enter (Item onClick) → navigate(`/${id}`).
 */
export function Cmdk({ open, onOpenChange }: CmdkProps) {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");

  const items: SectionItem[] = useMemo(
    () =>
      SECTIONS.map((s) => ({
        id: s.id,
        label: s.label,
        value: s.id,
      })),
    [],
  );

  useEffect(() => {
    if (!open) setQuery("");
  }, [open]);

  function jump(id: SectionId) {
    onOpenChange(false);
    void navigate({ to: `/${id}` });
  }

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Portal>
        <Dialog.Backdrop className="cmdk-backdrop" />
        <Dialog.Viewport className="cmdk-viewport">
          <Dialog.Popup className="cmdk" aria-label="Jump to section">
            <Dialog.Title className="sr-only">Jump to section</Dialog.Title>
            <Dialog.Description className="sr-only">
              Filter and jump to an admin panel
            </Dialog.Description>
            <Dialog.Close className="sr-only">Close</Dialog.Close>
            <Autocomplete.Root
              items={items}
              value={query}
              onValueChange={(v) => {
                setQuery(v);
              }}
              itemToStringValue={(item) =>
                typeof item === "string" ? item : item.label
              }
              autoHighlight="always"
              keepHighlight
              open
              inline
              mode="list"
            >
              <Autocomplete.Input
                className="cmdk__input"
                placeholder="Jump to section…"
                aria-label="Jump to section"
              />
              <Autocomplete.List className="cmdk__list">
                {(item: SectionItem) => (
                  <Autocomplete.Item
                    key={item.id}
                    value={item}
                    className="cmdk__item"
                    onClick={() => {
                      jump(item.id);
                    }}
                  >
                    {item.label}
                    <span className="cmdk__hint">#{item.id}</span>
                  </Autocomplete.Item>
                )}
              </Autocomplete.List>
              <Autocomplete.Empty className="cmdk__empty">
                No matches
              </Autocomplete.Empty>
            </Autocomplete.Root>
          </Dialog.Popup>
        </Dialog.Viewport>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
