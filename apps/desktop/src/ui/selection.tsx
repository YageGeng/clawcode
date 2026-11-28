import { signal, Text, View } from "nsh";
import type { ViewRef } from "nsh";
import { Button as BaseButton, Checkbox as BaseCheckbox, Select as BaseSelect, Slider as BaseSlider, Switch as BaseSwitch } from "nsh/base";
import type { SelectItem } from "nsh/base";

import type { ControlSize } from "./button";
import { Icon } from "./icon";
import type { IconName } from "./icon";
import { popupSurface } from "./menu";
import { motionClass } from "./motion";
import { focusRing, inlineLabel, type } from "./tokens";

/** A checkbox with its label; the app owns `checked`. */
export function Checkbox(props: { label: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void }) {
  return (
    <BaseCheckbox
      checked={props.checked}
      disabled={props.disabled === true}
      onChange={(event) => props.onChange(event.checked)}
      class={`group flex-row items-center gap-2 rounded-(--radius) py-0.5 ${focusRing} disabled:opacity-50`}
      indicatorClass={`size-4 items-center justify-center rounded-[5px] border border-input bg-background group-hover:border-foreground/40 group-active:scale-[0.95] group-disabled:hover:border-input group-disabled:active:scale-100 checked:border-primary checked:bg-primary group-hover:checked:bg-primary/90 ${motionClass.wash}`}
      indicator={
        // Toggle checkmark visibility immediately without an opacity fade.
        <View class={props.checked ? "opacity-100" : "opacity-0"}>
          <Icon name="check" size="sm" color="primary-foreground" />
        </View>
      }
    >
      <Text class={`${inlineLabel} ${type.ui} text-foreground`}>{props.label}</Text>
    </BaseCheckbox>
  );
}

/** An on/off switch with its label; the app owns `checked`. */
export function Switch(props: { label: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void }) {
  return (
    <BaseSwitch
      accessibilityLabel={props.label}
      checked={props.checked}
      disabled={props.disabled === true}
      onChange={(event) => props.onChange(event.checked)}
      class={`group flex-row items-center gap-2.5 rounded-(--radius) py-0.5 ${focusRing} disabled:opacity-50`}
      trackClass={`h-5 w-9 justify-center rounded-full bg-input px-0.5 group-hover:bg-foreground/25 group-active:scale-[0.97] group-disabled:hover:bg-input group-disabled:active:scale-100 checked:bg-primary group-hover:checked:bg-primary/90 ${motionClass.wash}`}
      thumbClass={`size-4 rounded-full bg-background shadow-xs checked:translate-x-4 ${motionClass.move}`}
    >
      <Text class={`${inlineLabel} ${type.ui} text-foreground`}>{props.label}</Text>
    </BaseSwitch>
  );
}

/** Estimates the minimum popup width in points so compact triggers never clip wider option labels. */
function estimatePopupMinWidth(items: readonly SelectItem[]): number {
  let widest = 0;
  for (const item of items) {
    let width = 0;
    for (const ch of item.title) {
      const code = ch.charCodeAt(0);
      // CJK/fullwidth glyphs (~14.5pt) vs uppercase Latin (~8.8pt) vs regular ASCII/punctuation (~7.6pt).
      width += code > 0x2e7f ? 14.5 : code >= 65 && code <= 90 ? 8.8 : 7.6;
    }
    if (width > widest) widest = width;
  }
  // Popup border + p-1.5 (14pt) + item px-2.5 (20pt) + gap-2 (8pt) + check icon size-4 (16pt) + safety room (14pt).
  return Math.min(360, Math.max(140, Math.ceil(widest + 72)));
}

/** One value chosen from a list; the popup is at least as wide as its trigger and wide enough for its options. */
export function Select(props: {
  items: SelectItem[];
  value: string | null;
  placeholder?: string;
  size?: ControlSize;
  variant?: "field" | "toolbar";
  disabled?: boolean;
  accessibilityLabel?: string;
  triggerRef?: (node: ViewRef) => void;
  onOpenChange?: (open: boolean) => void;
  class?: string;
  onChange: (value: string | null) => void;
}) {
  const title = () => props.items.find((item) => item.value === props.value)?.title ?? props.placeholder ?? "";
  return (
    <BaseSelect
      items={props.items}
      value={props.value}
      disabled={props.disabled === true}
      accessibilityLabel={props.accessibilityLabel}
      onChange={(event) => props.onChange(event.value)}
      onOpenChange={(event) => props.onOpenChange?.(event.open)}
      offset={6}
      margin={8}
      class={props.class}
      popupClass={popupSurface}
      popupStyle={{ minWidth: estimatePopupMinWidth(props.items) }}
      // Keep the outer Option row column-aligned (`stretch` cross-axis) so NSH's Renderer slot wrapper spans full width.
      itemClass={`group h-8 justify-center rounded-(--radius) px-2.5 focus:bg-accent hover:bg-accent disabled:opacity-50 ${motionClass.wash}`}
      renderItem={(entry) => (
        <View class="flex-row items-center justify-between gap-2">
          <Text class={`min-w-0 flex-1 truncate ${inlineLabel} ${type.ui} text-popover-foreground group-selected:font-medium`}>{entry.title}</Text>
          <View class="size-4 shrink-0 items-center justify-center opacity-0 group-selected:opacity-100">
            <Icon name="check" size="sm" />
          </View>
        </View>
      )}
      trigger={<SelectTrigger label={title()} size={props.size ?? "md"} variant={props.variant ?? "field"}
        triggerRef={props.triggerRef} disabled={props.disabled === true} />}
    />
  );
}

/** Trigger heights and padding per size, shared with `Input` so text lines up. */
const triggerSizing: Readonly<Record<ControlSize, string>> = { sm: "h-6 px-2", md: "h-8 px-2.5" };

/**
 * The trigger of a `Select`: an outline field with the chosen title and a chevron. A plain view, not a
 * `Button`: the select owns the press, the role and the keyboard of its trigger.
 */
function SelectTrigger(props: {
  label: string;
  size: ControlSize;
  disabled: boolean;
  variant: "field" | "toolbar";
  triggerRef?: (node: ViewRef) => void;
}) {
  return (
    <View
      ref={(node) => props.triggerRef?.(node)}
      class={`flex-row items-center justify-between rounded-(--radius) ${props.variant === "toolbar" ? "gap-1 bg-transparent" : "gap-2 border border-input bg-background"} hover:bg-accent ${focusRing} ${motionClass.wash} ${triggerSizing[props.size]} ${props.disabled ? "opacity-50" : ""}`}
    >
      <Text class={`shrink truncate ${inlineLabel} ${props.size === "sm" ? type.meta : type.ui} ${props.variant === "toolbar" ? "font-medium text-muted-foreground" : "text-foreground"}`}>{props.label}</Text>
      <View class="shrink-0">
        <Icon name="chevron-down" size="sm" color="muted-foreground" />
      </View>
    </View>
  );
}

/** A discrete slider whose thumb previews a named choice and commits it once on release or a keyboard step. */
export function StepSlider(props: {
  label: string;
  items: readonly SelectItem[];
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  // Remember the option itself through a drag. If the host replaces its list, an old index cannot select a new option.
  const drag = signal<{ items: readonly SelectItem[]; value: string } | null>(null);
  const index = () => {
    const preview = drag();
    const value = preview && !props.disabled && preview.items === props.items ? preview.value : props.value;
    return props.items.findIndex((item) => item.value === value);
  };
  const title = () => props.items[index()]?.title ?? props.value;
  return (
    <View class="gap-1">
      <View class="flex-row items-center justify-between gap-2">
        <Text class={`${type.meta} font-medium text-foreground`}>{props.label}</Text>
        <Text class={`shrink truncate ${type.meta} text-muted-foreground`}>{title()}</Text>
      </View>
      <BaseSlider
        min={0} max={Math.max(0, props.items.length - 1)} step={1} value={Math.max(0, index())}
        disabled={props.disabled}
        accessibilityLabel={`${props.label}: ${title()}`}
        class={`mx-2 ${props.disabled ? "opacity-50" : ""}`}
        trackClass="h-6 justify-center"
        indicatorClass="h-1.5 rounded-full bg-muted"
        rangeClass="rounded-full bg-primary"
        thumbClass={`size-4 rounded-full border border-primary bg-background hover:bg-accent ${focusRing} ${motionClass.wash}`}
        onChange={({ value }) => {
          const choice = typeof value === "number" ? props.items[value] : undefined;
          if (!props.disabled && choice) drag.set({ items: props.items, value: choice.value });
        }}
        onChangeEnd={({ value }) => {
          const preview = drag();
          drag.set(null);
          const choice = typeof value === "number" ? props.items[value] : undefined;
          if (!props.disabled && choice && (!preview || preview.items === props.items)) props.onChange(choice.value);
        }}
      />
      <View accessibilityElementsHidden class="mx-2 flex-row justify-between">
        <Each of={props.items} key={(item) => item.value}>
          {() => <View class="h-1 w-px bg-input" />}
        </Each>
      </View>
      <View accessibilityElementsHidden class="flex-row justify-between gap-2">
        <Text class={`shrink truncate ${type.meta} text-muted-foreground`}>{props.items[0]?.title}</Text>
        <Text class={`shrink truncate ${type.meta} text-muted-foreground`}>{props.items[props.items.length - 1]?.title}</Text>
      </View>
    </View>
  );
}

/** One segment of a `Segmented` control. */
export type SegmentedItem = Readonly<{ value: string; label: string; icon?: IconName }>;

/** A segmented control: a few exclusive choices side by side in one track, the chosen one raised. */
export function Segmented(props: { items: readonly SegmentedItem[]; value: string; accessibilityLabel: string; class?: string; onChange: (value: string) => void }) {
  return (
    <View role="radiogroup" accessibilityLabel={props.accessibilityLabel}
      class={`h-7 flex-row items-center gap-0.5 rounded-(--radius) bg-muted p-0.5 ${props.class ?? ""}`}>
      <Each of={props.items} key={(item) => item.value}>
        {(item) => {
          const active = () => props.value === item().value;
          return (
            <BaseButton
              role="radio"
              accessibilityLabel={item().label}
              accessibilityState={{ checked: active() }}
              selected={active()}
              onPress={() => props.onChange(item().value)}
              // `bg-card` + hairline border reads as raised above `bg-muted` in both light and dark themes; the press scale and the
              // washes share one transition (`motionClass.press`).
              class={`h-6 flex-row items-center justify-center gap-1.5 rounded-[8px] border border-transparent px-2.5 hover:bg-accent selected:border-border/70 selected:bg-card selected:shadow-xs ${focusRing} ${motionClass.press}`}
            >
              <If when={item().icon}>
                {(icon) => <Icon name={icon()} size="sm" color={active() ? "foreground" : "muted-foreground"} />}
              </If>
              <Text class={`${inlineLabel} ${type.meta} font-medium ${active() ? "text-foreground" : "text-muted-foreground"}`}>{item().label}</Text>
            </BaseButton>
          );
        }}
      </Each>
    </View>
  );
}
