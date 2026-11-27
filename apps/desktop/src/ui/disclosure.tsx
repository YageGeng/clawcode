import { Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconName } from "./icon";
import { motionClass } from "./motion";
import { focusRing, inlineLabel, type } from "./tokens";

/** A grey one-line toggle ("Worked for 3m ›", "Ran commands ›") with its content below while open; the caller owns `open`. */
export function Disclosure(props: {
  label: string;
  /** What screen readers say when it differs from `label` (a ticking duration is not re-announced every second). */
  accessibilityLabel?: string;
  accessibilityHint?: string;
  icon?: IconName;
  open: boolean;
  onToggle: () => void;
  /** Shifts the trigger padding left (`-ml-2`) so top-level disclosures align optically with prose. */
  flush?: boolean;
  /** Shown after the label (a breathing caret while work runs). */
  trailing?: JSX.Element;
  /** An independent action at the end of a full-width summary bar. */
  action?: JSX.Element;
  children?: JSX.Element;
}) {
  // A JSX slot is a getter. Check its presence without constructing the action twice.
  const summary = "action" in props;
  const trigger = (
    // NSH's disclosure trigger exposes expanded on the same focusable view that receives the press.
    <View
      role="button"
      focusable
      onPress={props.onToggle}
      accessibilityState={{ expanded: props.open }}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
      accessibilityHint={props.accessibilityHint}
      class={`group flex-row items-center gap-1.5 rounded-(--radius) px-2 py-1 ${summary ? "min-w-0 flex-1 self-stretch" : "self-start"} ${props.flush ? "-ml-2" : ""} hover:bg-accent active:bg-sidebar-accent/80 ${focusRing} ${motionClass.wash}`}
    >
      <If when={props.icon}>{(icon) => <Icon name={icon()} size="sm" color="muted-foreground" />}</If>
      <Text class={`tabular-nums ${inlineLabel} ${type.ui} ${summary ? "shrink truncate text-foreground" : "text-muted-foreground group-hover:text-foreground"}`}>{props.label}</Text>
      {props.trailing}
      <View class={`shrink-0 ${props.open ? "rotate-90" : ""} ${motionClass.turn}`}>
        <Icon name="chevron-right" size="sm" color="muted-foreground" />
      </View>
    </View>
  );
  return (
    <View class="gap-2">
      {summary ? (
        <View class="min-h-10 flex-row items-center gap-1 rounded-(--radius) border border-border bg-muted/60 py-1 pl-1 pr-2">
          {trigger}
          {props.action}
        </View>
      ) : trigger}
      <If when={props.open}>{props.children}</If>
    </View>
  );
}
