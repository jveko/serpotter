import { Dialog as BaseDialog } from "@base-ui/react/dialog";
import type { ComponentProps } from "react";

/** Cobalt-wrapped Base UI Dialog. */
export const Dialog = {
  Root: BaseDialog.Root,
  Trigger: BaseDialog.Trigger,
  Portal: BaseDialog.Portal,
  Close: BaseDialog.Close,
  Title: BaseDialog.Title,
  Description: BaseDialog.Description,
  Backdrop: function DialogBackdrop(
    props: ComponentProps<typeof BaseDialog.Backdrop>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseDialog.Backdrop
        {...rest}
        className={["ui-dialog__backdrop", className].filter(Boolean).join(" ")}
      />
    );
  },
  Viewport: function DialogViewport(
    props: ComponentProps<typeof BaseDialog.Viewport>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseDialog.Viewport
        {...rest}
        className={["ui-dialog__viewport", className].filter(Boolean).join(" ")}
      />
    );
  },
  Popup: function DialogPopup(
    props: ComponentProps<typeof BaseDialog.Popup>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseDialog.Popup
        {...rest}
        className={["ui-dialog__popup", className].filter(Boolean).join(" ")}
      />
    );
  },
};
