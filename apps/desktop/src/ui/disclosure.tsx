import { Text, View } from "nsh";
import { Button as BaseButton } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconName } from "./icon";
import { motionClass } from "./motion";
import { focusRing, type } from "./tokens";

/** A grey one-line toggle ("Worked for 3m ›", "Ran commands ›") with its content below while open; the caller owns `open`. */
export function Disclosure(props: {
  label: string;
  icon?: IconName;
  open: boolean;
  onToggle: () => void;
  /** Shifts the trigger padding left (`-ml-2`) so top-level disclosures align optically with prose. */
  flush?: boolean;
  /** Shown after the label (a breathing caret while work runs). */
  trailing?: JSX.Element;
  children?: JSX.Element;
}) {
  return (
    <View class="gap-2">
      <BaseButton
        onPress={props.onToggle}
        selected={props.open}
        accessibilityLabel={props.label}
        class={`group flex-row items-center gap-1.5 self-start rounded-(--radius) px-2 py-1 ${props.flush ? "-ml-2" : ""} hover:bg-accent ${focusRing} ${motionClass.wash}`}
      >
        <If when={props.icon}>{(icon) => <Icon name={icon()} size="sm" color="muted-foreground" />}</If>
        <Text class={`${type.ui} text-muted-foreground group-hover:text-foreground`}>{props.label}</Text>
        {props.trailing}
        <View class={`${props.open ? "rotate-90" : ""} ${motionClass.turn}`}>
          <Icon name="chevron-right" size="sm" color="muted-foreground" />
        </View>
      </BaseButton>
      <If when={props.open}>{props.children}</If>
    </View>
  );
}
