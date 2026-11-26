import { Text, View } from "nsh";
import { Checkbox as BaseCheckbox, Select as BaseSelect, Switch as BaseSwitch } from "nsh/base";
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
        <View class={`${motionClass.swap} ${props.checked ? "opacity-100" : "opacity-0"}`}>
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
  disabled?: boolean;
  accessibilityLabel?: string;
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
      offset={4}
      class={props.class}
      popupClass={popupSurface}
      popupStartingClass={motionClass.popupStarting}
      popupEndingClass={motionClass.popupEnding}
      itemClass={`h-8 flex-row items-center rounded-(--radius) px-2.5 ${type.ui} text-popover-foreground focus:bg-accent selected:font-medium disabled:opacity-50 ${motionClass.wash}`}
      trigger={<SelectTrigger label={title()} size={props.size ?? "md"} disabled={props.disabled === true} />}
    />
  );
}

/** Trigger heights and padding per size, shared with `Input` so text lines up. */
const triggerSizing: Readonly<Record<ControlSize, string>> = { sm: "h-6 px-2", md: "h-8 px-2.5" };

/**
 * The trigger of a `Select`: an outline field with the chosen title and a chevron. A plain view, not a
 * `Button`: the select owns the press, the role and the keyboard of its trigger.
 */
function SelectTrigger(props: { label: string; size: ControlSize; disabled: boolean }) {
  return (
    <View
      class={`flex-row items-center justify-between gap-2 rounded-(--radius) border border-input bg-background hover:bg-accent ${focusRing} ${motionClass.wash} ${triggerSizing[props.size]} ${props.disabled ? "opacity-50" : ""}`}
    >
      <Text class={`${props.size === "sm" ? type.meta : type.ui} text-foreground truncate`}>{props.label}</Text>
      <Icon name="chevron-down" size="sm" color="muted-foreground" />
    </View>
  );
}
