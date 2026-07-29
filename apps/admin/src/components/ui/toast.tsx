import { Toast as BaseToast } from "@base-ui/react/toast";
import type { ComponentProps, ReactNode } from "react";

/** Shared manager so MutationCache can toast outside React. */
export const toastManager = BaseToast.createToastManager();

export const Toast = {
  Provider: function ToastProvider(props: ComponentProps<typeof BaseToast.Provider>) {
    const { toastManager: tm = toastManager, limit = 3, timeout = 4500, ...rest } = props;
    return <BaseToast.Provider toastManager={tm} limit={limit} timeout={timeout} {...rest} />;
  },
  Portal: BaseToast.Portal,
  Viewport: function ToastViewport(props: ComponentProps<typeof BaseToast.Viewport>) {
    const { className, ...rest } = props;
    return (
      <BaseToast.Viewport
        {...rest}
        className={["ui-toast__viewport", className].filter(Boolean).join(" ")}
      />
    );
  },
  Root: function ToastRoot(props: ComponentProps<typeof BaseToast.Root>) {
    const { className, toast, ...rest } = props;
    const type = toast?.type ?? "info";
    return (
      <BaseToast.Root
        {...rest}
        toast={toast}
        className={["ui-toast", `ui-toast--${type}`, className].filter(Boolean).join(" ")}
      />
    );
  },
  Content: function ToastContent(props: ComponentProps<typeof BaseToast.Content>) {
    const { className, ...rest } = props;
    return (
      <BaseToast.Content
        {...rest}
        className={["ui-toast__content", className].filter(Boolean).join(" ")}
      />
    );
  },
  Title: function ToastTitle(props: ComponentProps<typeof BaseToast.Title>) {
    const { className, ...rest } = props;
    return (
      <BaseToast.Title
        {...rest}
        className={["ui-toast__title", className].filter(Boolean).join(" ")}
      />
    );
  },
  Description: function ToastDescription(props: ComponentProps<typeof BaseToast.Description>) {
    const { className, ...rest } = props;
    return (
      <BaseToast.Description
        {...rest}
        className={["ui-toast__desc", className].filter(Boolean).join(" ")}
      />
    );
  },
  Close: function ToastClose(props: ComponentProps<typeof BaseToast.Close>) {
    const { className, children = "×", ...rest } = props;
    return (
      <BaseToast.Close
        {...rest}
        className={["ui-toast__close", className].filter(Boolean).join(" ")}
      >
        {children}
      </BaseToast.Close>
    );
  },
  useToastManager: BaseToast.useToastManager,
  createToastManager: BaseToast.createToastManager,
};

/** Renders the live toast stack. Place inside Toast.Provider. */
export function ToastList() {
  const { toasts } = Toast.useToastManager();
  return (
    <>
      {toasts.map((t) => (
        <Toast.Root key={t.id} toast={t}>
          <Toast.Content>
            {t.title ? <Toast.Title>{t.title as ReactNode}</Toast.Title> : null}
            {t.description ? (
              <Toast.Description>{t.description as ReactNode}</Toast.Description>
            ) : null}
            <Toast.Close aria-label="Dismiss" />
          </Toast.Content>
        </Toast.Root>
      ))}
    </>
  );
}

export function showToast(opts: {
  title?: string;
  description?: string;
  type?: "success" | "error" | "info" | "warn";
  timeout?: number;
}) {
  toastManager.add({
    title: opts.title,
    description: opts.description,
    type: opts.type ?? "info",
    timeout: opts.timeout,
  });
}
