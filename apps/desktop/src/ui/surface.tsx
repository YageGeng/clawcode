import { Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { JSX } from "nsh/jsx-runtime";

import type { Tone } from "./tokens";
import { inlineLabel, toneFill, toneSoft, type } from "./tokens";
import { motionClass } from "./motion";

/** Each surface's look: a content card, or the floating composer card (spec §5.1). */
const surfaceVariant = {
  card: "rounded-(--radius-lg) border border-border bg-card p-4 gap-3 shadow-xs",
  floating: "rounded-(--radius-xl) border border-input bg-card px-3.5 pt-3 pb-2.5 gap-2 shadow-md",
} as const;

/** A container for a group of content: a `card` (default) or the `floating` composer card. */
export function Surface(props: { children: JSX.Element; variant?: keyof typeof surfaceVariant; class?: string }) {
  return <View class={`${surfaceVariant[props.variant ?? "card"]} ${props.class ?? ""}`}>{props.children}</View>;
}

/** A short state, count or classification; neutral unless the state carries a meaning. */
export function Badge(props: { label: string; tone?: Tone }) {
  return (
    <View class={`h-5 justify-center rounded-full px-2 ${toneSoft[props.tone ?? "neutral"]}`}>
      <Text class={`${inlineLabel} ${type.meta} font-medium`}>{props.label}</Text>
    </View>
  );
}

/** A divider between groups; vertical inside a row. */
export function Separator(props: { vertical?: boolean }) {
  return <View class={props.vertical === true ? "w-px self-stretch bg-border" : "h-px self-stretch bg-border"} />;
}

/**
 * A small status dot with its label (never color alone); pulses while `live`. With `hideLabel` the label is only
 * spoken, for dense rows where a nearby title already says what the dot belongs to.
 */
export function StatusDot(props: { tone: Tone; label: string; live?: boolean; hideLabel?: boolean }) {
  return (
    <View class="flex-row items-center gap-1.5" accessibilityLabel={props.label}>
      <View class={`size-2 rounded-full ${toneFill[props.tone]} ${props.live === true ? motionClass.pulse : ""}`} />
      <If when={props.hideLabel !== true}>
        <Text class={`${inlineLabel} ${type.meta} text-muted-foreground`}>{props.label}</Text>
      </If>
    </View>
  );
}

/** A diff's added and removed line counts. */
export function LineCounts(props: { added: number; removed: number }) {
  return (
    <View class="shrink-0 flex-row items-center gap-1.5" accessibilityLabel={t("changes.lineCounts", { added: props.added, removed: props.removed })}>
      <Text class={`${inlineLabel} ${type.meta} font-mono text-added`}>+{props.added}</Text>
      <Text class={`${inlineLabel} ${type.meta} font-mono text-removed`}>−{props.removed}</Text>
    </View>
  );
}
