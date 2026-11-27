import { signal, Text, View } from "nsh";
import type { ViewRef } from "nsh";
import { Checkbox as BaseCheckbox, Select as BaseSelect, Slider as BaseSlider, Switch as BaseSwitch } from "nsh/base";
import type { SelectItem } from "nsh/base";

import type { ControlSize } from "./button";
import { Icon } from "./icon";
import { popupSurface } from "./menu";
import { motionClass } from "./motion";
import { focusRing, type } from "./tokens";

/** A checkbox with its label; the app owns `checked`. */
export function Checkbox(props: { label: string; checked: boolean; disabled?: boolean; onChange: (checked: boolean) => void }) {
  return (
    <BaseCheckbox
      checked={props.checked}
      disabled={props.disabled === true}
      onChange={(event) => props.onChange(event.checked)}
      class={`flex-row items-center gap-2 rounded-(--radius) ${focusRing} disabled:opacity-50`}
      indicatorClass={`size-4 items-center justify-center rounded-[5px] border border-input bg-background checked:border-primary checked:bg-primary ${motionClass.wash}`}
      indicator={
        // Toggle checkmark visibility immediately without an opacity fade.
        <View class={props.checked ? "opacity-100" : "opacity-0"}>
          <Icon name="check" size="sm" color="primary-foreground" />
        </View>
      }
    >
      <Text class={`${type.ui} text-foreground`}>{props.label}</Text>
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
      class={`flex-row items-center gap-2 rounded-(--radius) ${focusRing} disabled:opacity-50`}
      trackClass={`h-5 w-9 justify-center rounded-full bg-input px-0.5 checked:bg-primary ${motionClass.wash}`}
      thumbClass={`size-4 rounded-full bg-background checked:translate-x-4 ${motionClass.move}`}
    >
      <Text class={`${type.ui} text-foreground`}>{props.label}</Text>
    </BaseSwitch>
  );
}

/** One value chosen from a list; the popup is as wide as the trigger and shares its padding. */
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
      offset={4}
      class={props.class}
      popupClass={popupSurface}
      itemClass={`h-8 flex-row items-center rounded-(--radius) px-2.5 ${type.ui} text-popover-foreground focus:bg-accent selected:font-medium disabled:opacity-50 ${motionClass.wash}`}
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
function SelectTrigger(props: { label: string; size: ControlSize; disabled: boolean; variant: "field" | "toolbar"; triggerRef?: (node: ViewRef) => void }) {
  return (
    <View
      ref={(node) => props.triggerRef?.(node)}
      class={`flex-row items-center justify-between rounded-(--radius) ${props.variant === "toolbar" ? "gap-1 bg-transparent" : "gap-2 border border-input bg-background"} hover:bg-accent ${focusRing} ${motionClass.wash} ${triggerSizing[props.size]} ${props.disabled ? "opacity-50" : ""}`}
    >
      <Text class={`shrink truncate ${props.size === "sm" ? type.meta : type.ui} ${props.variant === "toolbar" ? "font-medium text-muted-foreground" : "text-foreground"}`}>{props.label}</Text>
      <Icon name="chevron-down" size="sm" color="muted-foreground" />
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
