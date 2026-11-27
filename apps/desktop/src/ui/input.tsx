import { Text, View } from "nsh";
import type { InputRef } from "nsh";
import { Input as BaseInput } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import type { ControlSize } from "./button";
import { motionClass } from "./motion";
import { type } from "./tokens";

/** A field's frame: border, radius, focus ring and the invalid and disabled states. */
function frame(invalid: boolean): string {
  const border = invalid ? "border-destructive" : "border-input focus:border-ring";
  return `rounded-(--radius) border bg-background text-foreground placeholder:text-muted-foreground ${border} focus:ring-2 focus:ring-ring/40 disabled:opacity-50 ${motionClass.wash}`;
}

/** Height, padding and text size per size; selects and lists share the horizontal padding. */
const sizing: Readonly<Record<ControlSize, string>> = {
  sm: `h-6 px-2 ${type.meta}`,
  md: `h-8 px-3 ${type.ui}`,
};

/** Shared props of the app's text controls. */
type TextControlProps = {
  value: string;
  placeholder?: string;
  disabled?: boolean;
  invalid?: boolean;
  /** What screen readers call it when no `Field` label names it. */
  accessibilityLabel?: string;
  class?: string;
  onChange: (value: string) => void;
  /** The field took or lost the keyboard focus (for keys bound only while it has it). */
  onFocus?: () => void;
  onBlur?: () => void;
};

/** A one-line text field. */
export function Input(props: TextControlProps & { size?: ControlSize; onSubmit?: () => void }) {
  return (
    <BaseInput
      value={props.value}
      placeholder={props.placeholder}
      readonly={props.disabled === true}
      accessibilityLabel={props.accessibilityLabel}
      onChange={(event) => props.onChange(event.value)}
      onSubmit={() => props.onSubmit?.()}
      class={`${frame(props.invalid === true)} ${sizing[props.size ?? "md"]} ${props.class ?? ""}`}
    />
  );
}

/**
 * The heights a `Textarea` grows between, in points: `minLines` and `maxLines` of UI text (`type.ui`, a 20pt
 * line) plus the 16pt of vertical padding `py-2` adds.
 */
export function textareaHeights(minLines = 2, maxLines = 8): Readonly<{ min: number; max: number }> {
  const lineHeight = 20;
  const padding = 16;
  return { min: minLines * lineHeight + padding, max: maxLines * lineHeight + padding };
}

/**
 * A multi-line text field that grows with its text from `minLines` up to `maxLines`, then scrolls.
 * With `onSubmit`, Enter submits and Shift+Enter starts a new line; while an input method is composing,
 * Enter only confirms the composition. The `bare` variant draws no frame of its own, for a field inside a card
 * that is its frame (the composer).
 */
export function Textarea(
  props: TextControlProps & { minLines?: number; maxLines?: number; onSubmit?: () => void; inputRef?: (node: InputRef) => void; variant?: "field" | "bare" },
) {
  const heights = () => textareaHeights(props.minLines, props.maxLines);
  const look = () =>
    props.variant === "bare"
      ? "bg-transparent px-1 text-foreground placeholder:text-muted-foreground disabled:opacity-50"
      : `${frame(props.invalid === true)} px-3`;
  return (
    <BaseInput
      multiline
      ref={(node) => props.inputRef?.(node)}
      value={props.value}
      placeholder={props.placeholder}
      readonly={props.disabled === true}
      accessibilityLabel={props.accessibilityLabel}
      submitBehavior={props.onSubmit === undefined ? "newline" : "submit"}
      onChange={(event) => props.onChange(event.value)}
      onSubmit={() => props.onSubmit?.()}
      onFocus={() => props.onFocus?.()}
      onBlur={() => props.onBlur?.()}
      class={`${look()} py-2 ${type.ui} ${props.class ?? ""}`}
      style={{ minHeight: heights().min, maxHeight: heights().max }}
    />
  );
}

/** A labelled control: the label above, the description or the error below. */
export function Field(props: { label: string; description?: string; error?: string; children: JSX.Element; class?: string }) {
  return (
    <View class={`gap-1 ${props.class ?? ""}`}>
      <Text class={`${type.meta} font-medium text-foreground`}>{props.label}</Text>
      {props.children}
      <If when={props.error} else={<If when={props.description}>{(text) => <Text class={`${type.meta} text-muted-foreground`}>{text()}</Text>}</If>}>
        {(error) => <Text class={`${type.meta} text-destructive`}>{error()}</Text>}
      </If>
    </View>
  );
}
