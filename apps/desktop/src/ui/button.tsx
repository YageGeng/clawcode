import { Text } from "nsh";
import type { ViewRef } from "nsh";
import { Button as BaseButton } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconColor, IconName } from "./icon";
import { motionClass } from "./motion";
import { focusRing, type } from "./tokens";

/** A button's meaning: how loudly it asks to be pressed. */
export type ButtonVariant = "primary" | "secondary" | "ghost" | "outline" | "danger";

/** Control sizes on the desktop: `sm` 24pt, `md` 32pt. */
export type ControlSize = "sm" | "md";

/** Each variant's surface: rest, hover, press and selected washes, all tokens. */
const surface: Readonly<Record<ButtonVariant, string>> = {
  primary: "bg-primary hover:bg-primary/90 active:bg-primary/80 selected:bg-primary/80",
  secondary: "bg-secondary hover:bg-secondary/80 active:bg-secondary/60 selected:bg-secondary/60",
  ghost: "bg-transparent hover:bg-accent active:bg-accent/70 selected:bg-accent",
  outline: "border border-input bg-background hover:bg-accent active:bg-accent/70 selected:bg-accent",
  danger: "bg-destructive hover:bg-destructive/90 active:bg-destructive/80 selected:bg-destructive/80",
};

/** Each variant's text class and icon tint. */
const ink: Readonly<Record<ButtonVariant, { text: string; icon: IconColor }>> = {
  primary: { text: "text-primary-foreground", icon: "primary-foreground" },
  secondary: { text: "text-secondary-foreground", icon: "secondary-foreground" },
  ghost: { text: "text-foreground", icon: "foreground" },
  outline: { text: "text-foreground", icon: "foreground" },
  danger: { text: "text-destructive-foreground", icon: "destructive-foreground" },
};

/** What every button shares: layout, focus ring, disabled look (the radius is per kind). */
const shell = `flex-row items-center justify-center ${focusRing} disabled:opacity-50`;

/** Height, padding, gap and text size per size: one scale drives them together. */
const sizing: Readonly<Record<ControlSize, string>> = {
  sm: "h-6 px-2 gap-1",
  md: "h-8 px-3 gap-1.5",
};
const textSize: Readonly<Record<ControlSize, string>> = { sm: type.meta, md: type.ui };

/** Shared props of the app's buttons. */
type ButtonBaseProps = {
  variant?: ButtonVariant;
  size?: ControlSize;
  disabled?: boolean;
  selected?: boolean;
  /** Shows a spinner in place of the icon and takes no presses; the size stays. */
  loading?: boolean;
  /** Layout from the caller only: width, flex, margin, alignment. */
  class?: string;
  buttonRef?: (node: ViewRef) => void;
  onPress?: () => void;
};

/** The app's text button, with optional leading and trailing icons. */
export function Button(props: ButtonBaseProps & { label: string; accessibilityLabel?: string; icon?: IconName; trailingIcon?: IconName }) {
  const variant = () => props.variant ?? "secondary";
  const size = () => props.size ?? "md";
  return (
    <BaseButton
      ref={(node) => props.buttonRef?.(node)}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      disabled={props.disabled === true || props.loading === true}
      selected={props.selected === true}
      onPress={props.onPress}
      class={`${shell} rounded-(--radius) ${motionClass.wash} ${motionClass.press} ${surface[variant()]} ${sizing[size()]} ${props.class ?? ""}`}
    >
      <If when={props.loading === true}>
        <Icon name="loader-circle" size="sm" color={ink[variant()].icon} class={motionClass.spin} />
      </If>
      <If when={props.loading !== true && props.icon !== undefined}>
        <Icon name={props.icon ?? "plus"} size="sm" color={ink[variant()].icon} />
      </If>
      <Text class={`shrink truncate ${ink[variant()].text} ${textSize[size()]} font-medium`}>{props.label}</Text>
      <If when={props.trailingIcon !== undefined}>
        <Icon name={props.trailingIcon ?? "chevron-down"} size="sm" color={variant() === "ghost" || variant() === "outline" ? "muted-foreground" : ink[variant()].icon} />
      </If>
    </BaseButton>
  );
}

/** Square sizes of an icon button: the control height. */
const square: Readonly<Record<ControlSize, string>> = { sm: "size-6", md: "size-8" };

/**
 * An icon-only button: a square of the control height, or a circle when `round` (the composer's send, a floating
 * jump button); `label` is what screen readers and the tooltip say.
 */
export function IconButton(props: ButtonBaseProps & { icon: IconName; label: string; tooltip?: JSX.Element; round?: boolean }) {
  const variant = () => props.variant ?? "ghost";
  const size = () => props.size ?? "md";
  return (
    <BaseButton
      ref={(node) => props.buttonRef?.(node)}
      accessibilityLabel={props.label}
      disabled={props.disabled === true || props.loading === true}
      selected={props.selected === true}
      onPress={props.onPress}
      tooltip={props.tooltip}
      class={`${shell} ${props.round === true ? "rounded-full" : "rounded-(--radius)"} ${motionClass.wash} ${motionClass.pressIcon} ${surface[variant()]} ${square[size()]} ${props.class ?? ""}`}
    >
      <Icon
        name={props.loading === true ? "loader-circle" : props.icon}
        size={size() === "sm" ? "sm" : "md"}
        color={ink[variant()].icon}
        class={props.loading === true ? motionClass.spin : undefined}
      />
    </BaseButton>
  );
}
