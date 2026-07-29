import { AlertDialog as BaseAlertDialog } from "@base-ui/react/alert-dialog";
import type { ComponentProps, ReactNode } from "react";

/** Cobalt-wrapped Base UI AlertDialog. */
export const AlertDialog = {
  Root: BaseAlertDialog.Root,
  Trigger: BaseAlertDialog.Trigger,
  Portal: BaseAlertDialog.Portal,
  Close: BaseAlertDialog.Close,
  Title: function AlertDialogTitle(
    props: ComponentProps<typeof BaseAlertDialog.Title>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseAlertDialog.Title
        {...rest}
        className={["ui-alert__title", className].filter(Boolean).join(" ")}
      />
    );
  },
  Description: function AlertDialogDescription(
    props: ComponentProps<typeof BaseAlertDialog.Description>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseAlertDialog.Description
        {...rest}
        className={["ui-alert__desc", className].filter(Boolean).join(" ")}
      />
    );
  },
  Backdrop: function AlertDialogBackdrop(
    props: ComponentProps<typeof BaseAlertDialog.Backdrop>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseAlertDialog.Backdrop
        {...rest}
        className={["ui-dialog__backdrop", className].filter(Boolean).join(" ")}
      />
    );
  },
  Viewport: function AlertDialogViewport(
    props: ComponentProps<typeof BaseAlertDialog.Viewport>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseAlertDialog.Viewport
        {...rest}
        className={["ui-dialog__viewport", className].filter(Boolean).join(" ")}
      />
    );
  },
  Popup: function AlertDialogPopup(
    props: ComponentProps<typeof BaseAlertDialog.Popup>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseAlertDialog.Popup
        {...rest}
        className={["ui-dialog__popup ui-alert__popup", className]
          .filter(Boolean)
          .join(" ")}
      />
    );
  },
};

export type ConfirmDeleteProps = {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: ReactNode;
  confirmLabel?: string;
  busy?: boolean;
  onConfirm: () => void;
};

/** Controlled destructive confirm. */
export function ConfirmDeleteDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel = "Delete",
  busy = false,
  onConfirm,
}: ConfirmDeleteProps) {
  return (
    <AlertDialog.Root open={open} onOpenChange={onOpenChange}>
      <AlertDialog.Portal>
        <AlertDialog.Backdrop />
        <AlertDialog.Viewport>
          <AlertDialog.Popup>
            <AlertDialog.Title>{title}</AlertDialog.Title>
            <AlertDialog.Description>{description}</AlertDialog.Description>
            <div className="ui-alert__actions">
              <AlertDialog.Close
                className="btn btn--ghost btn--sm"
                disabled={busy}
              >
                Cancel
              </AlertDialog.Close>
              <button
                type="button"
                className="btn btn--danger btn--sm"
                disabled={busy}
                data-state={busy ? "loading" : undefined}
                onClick={() => onConfirm()}
              >
                {confirmLabel}
              </button>
            </div>
          </AlertDialog.Popup>
        </AlertDialog.Viewport>
      </AlertDialog.Portal>
    </AlertDialog.Root>
  );
}
