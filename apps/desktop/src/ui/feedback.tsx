import { Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconName } from "./icon";
import { type } from "./tokens";

/** An empty state: what is missing and the action that fills it. */
export function Empty(props: { title: string; description?: string; icon?: IconName; action?: JSX.Element }) {
  return (
    <View class="items-center justify-center gap-2 p-8">
      <If when={props.icon}>{(icon) => <Icon name={icon()} color="muted-foreground" />}</If>
      <Text class={`${type.ui} font-medium text-foreground`}>{props.title}</Text>
      <If when={props.description}>{(text) => <Text class={`${type.meta} text-muted-foreground text-center`}>{text()}</Text>}</If>
      {props.action}
    </View>
  );
}

/** A placeholder block while content loads: pulses slowly, rests under reduced motion. */
export function Skeleton(props: { class?: string }) {
  return <View accessibilityElementsHidden class={`rounded-(--radius) bg-muted animate-pulse motion-reduce:animate-none ${props.class ?? "h-4 w-full"}`} />;
}

/** An indeterminate progress mark; `label` is what screen readers say. */
export function Spinner(props: { label: string; size?: "sm" | "md" }) {
  return (
    <View accessibilityLabel={props.label} role="progressbar" class="animate-spin motion-reduce:animate-none">
      <Icon name="loader-circle" size={props.size ?? "md"} color="muted-foreground" />
    </View>
  );
}
