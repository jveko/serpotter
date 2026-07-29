import { Menu as BaseMenu } from "@base-ui/react/menu";
import type { ComponentProps } from "react";

/** Cobalt-wrapped Base UI Menu (optional row overflow). */
export const Menu = {
  Root: BaseMenu.Root,
  Trigger: function MenuTrigger(
    props: ComponentProps<typeof BaseMenu.Trigger>,
  ) {
    const { className, ...rest } = props;
    return (
      <BaseMenu.Trigger
        {...rest}
        className={["ui-menu__trigger", className].filter(Boolean).join(" ")}
      />
    );
  },
  Portal: BaseMenu.Portal,
  Positioner: BaseMenu.Positioner,
  Popup: function MenuPopup(props: ComponentProps<typeof BaseMenu.Popup>) {
    const { className, ...rest } = props;
    return (
      <BaseMenu.Popup
        {...rest}
        className={["ui-menu__popup", className].filter(Boolean).join(" ")}
      />
    );
  },
  Item: function MenuItem(props: ComponentProps<typeof BaseMenu.Item>) {
    const { className, ...rest } = props;
    return (
      <BaseMenu.Item
        {...rest}
        className={["ui-menu__item", className].filter(Boolean).join(" ")}
      />
    );
  },
  Separator: BaseMenu.Separator,
  Group: BaseMenu.Group,
  GroupLabel: BaseMenu.GroupLabel,
};
