import { Text, View } from "nsh";
import {
  Menu as BaseMenu,
  MenuItem as BaseMenuItem,
  MenuSeparator as BaseMenuSeparator,
  Popover as BasePopover,
  Submenu as BaseSubmenu,
  Tooltip as BaseTooltip,
} from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconColor, IconName } from "./icon";
import { motionClass } from "./motion";
import { type } from "./tokens";

/** The one popup surface menus, popovers and selects share (spec §5.1: one popover look); opens and closes immediately without fade. */
export const popupSurface = "rounded-(--radius-lg) border border-border bg-popover p-1.5 shadow-lg";

/** A menu its trigger opens immediately; Escape and a press outside close it. An optional `title` heads it. */
export function Menu(props: {
  trigger: JSX.Element; children: JSX.Element; title?: string; align?: "start" | "end"; class?: string;
  open?: boolean; onOpenChange?: (open: boolean) => void;
}) {
  return (
    <BaseMenu
      trigger={props.trigger}
      open={props.open}
      onOpenChange={(event) => props.onOpenChange?.(event.open)}
      align={props.align ?? "start"}
      offset={6}
      popupClass={`min-w-48 ${popupSurface} ${props.class ?? ""}`}
    >
      <If when={props.title}>
        {(title) => <Text class={`px-2.5 pt-1 pb-1.5 ${type.meta} text-muted-foreground`}>{title()}</Text>}
      </If>
      {props.children}
    </BaseMenu>
  );
}

/** The row every menu item and submenu trigger shares. */
const itemRow = `min-h-8 flex-row items-center gap-2.5 rounded-(--radius) px-2.5 py-1.5 focus:bg-accent hover:bg-accent disabled:opacity-50 ${motionClass.wash}`;

/**
 * One command of a `Menu`. `danger` for destructive commands (red), `warning` for risky but non-destructive
 * choices (amber, e.g. full access), `checked` for the setting it shows; `description` adds a grey line under it.
 */
export function MenuItem(props: {
  label: string;
  description?: string;
  icon?: IconName;
  danger?: boolean;
  warning?: boolean;
  disabled?: boolean;
  checked?: boolean;
  shortcut?: string;
  onSelect: () => void;
}) {
  const tone = (): { text: string; icon: IconColor } =>
    props.danger === true
      ? { text: "text-destructive", icon: "destructive" }
      : props.warning === true
        ? { text: "text-warning", icon: "warning" }
        : { text: "text-popover-foreground", icon: "foreground" };
  return (
    <BaseMenuItem disabled={props.disabled === true} checked={props.checked} onSelect={props.onSelect} class={itemRow}>
      <If when={props.icon}>{(icon) => <Icon name={icon()} size="md" color={tone().icon} />}</If>
      <View class="flex-1 gap-0.5">
        <Text class={`${type.ui} ${tone().text}`}>{props.label}</Text>
        <If when={props.description}>
          {(text) => <Text class={`${type.meta} ${props.warning === true || props.danger === true ? tone().text : "text-muted-foreground"}`}>{text()}</Text>}
        </If>
      </View>
      <If when={props.shortcut}>{(keys) => <Text class={`${type.meta} text-muted-foreground`}>{keys()}</Text>}</If>
      <View class="size-4 items-center justify-center">
        <If when={props.checked === true}>
          <Icon name="check" size="sm" />
        </If>
      </View>
    </BaseMenuItem>
  );
}

/** A divider between groups of menu items. */
export function MenuSeparator() {
  return <BaseMenuSeparator class="mx-1 my-1 h-px bg-border" />;
}

/** A nested menu that cascades beside its row. */
export function Submenu(props: { label: string; icon?: IconName; children: JSX.Element }) {
  return (
    <BaseSubmenu
      offset={6}
      trigger={
        <View class={itemRow}>
          <If when={props.icon}>{(icon) => <Icon name={icon()} size="md" />}</If>
          <Text class={`flex-1 ${type.ui} text-popover-foreground`}>{props.label}</Text>
          <Icon name="chevron-right" size="sm" color="muted-foreground" />
        </View>
      }
    >
      {props.children}
    </BaseSubmenu>
  );
}

/** Content anchored to its trigger: same instant popup surface as a menu. */
export function Popover(props: {
  trigger: JSX.Element;
  children: JSX.Element;
  class?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  onOpen?: () => void;
}) {
  return (
    <BasePopover
      trigger={props.trigger}
      open={props.open}
      onOpenChange={(event) => props.onOpenChange?.(event.open)}
      onOpen={props.onOpen}
      offset={6}
      popupClass={`${popupSurface} p-3 ${props.class ?? ""}`}
    >
      {props.children}
    </BasePopover>
  );
}

/** A short hint, given as an element's `tooltip`; shows after a rest or at once on keyboard focus. */
export function Tooltip(props: { text: string }) {
  return (
    <BaseTooltip
      class={`rounded-(--radius) bg-foreground px-2 py-1 ${motionClass.tooltip}`}
      startingClass={motionClass.tooltipStarting}
      endingClass={motionClass.tooltipEnding}
    >
      <Text class={`${type.meta} text-background`}>{props.text}</Text>
    </BaseTooltip>
  );
}
