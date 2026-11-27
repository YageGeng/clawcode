import { Text, View } from "nsh";
import type { JSX } from "nsh/jsx-runtime";

import { Icon } from "./icon";
import type { IconName } from "./icon";
import { type } from "./tokens";
import { motionClass } from "./motion";

/** An empty state: what is missing and the action that fills it. */
export function Empty(props: { title: string; description?: string; icon?: IconName; action?: JSX.Element; class?: string }) {
  return (
    <View class={`items-center justify-center gap-2 p-6 ${props.class ?? ""}`}>
      <If when={props.icon}>
        {(icon) => (
          <View class="mb-0.5 size-9 items-center justify-center rounded-(--radius) bg-muted/70">
            <Icon name={icon()} color="muted-foreground" />
          </View>
        )}
      </If>
      <Text class={`${type.ui} font-medium text-foreground text-center`}>{props.title}</Text>
      <If when={props.description}>{(text) => <Text class={`${type.meta} text-muted-foreground text-center`}>{text()}</Text>}</If>
      {props.action}
    </View>
  );
}

/** A placeholder block while content loads: pulses slowly, rests under reduced motion. */
export function Skeleton(props: { class?: string }) {
  return <View accessibilityElementsHidden class={`rounded-(--radius) bg-muted ${motionClass.pulse} ${props.class ?? "h-4 w-full"}`} />;
}

/** An indeterminate progress mark; `label` is what screen readers say. */
export function Spinner(props: { label: string; size?: "sm" | "md" }) {
  return (
    <View accessibilityLabel={props.label} role="progressbar" class={motionClass.spin}>
      <Icon name="loader-circle" size={props.size ?? "md"} color="muted-foreground" />
    </View>
  );
}
