import { cleanup, Clipboard, signal, Text, View } from "nsh";
import type { ClipboardWrite } from "nsh";
import { t } from "nsh/i18n";

import { IconButton } from "./button";
import type { ControlSize } from "./button";
import { inlineLabel, type } from "./tokens";

/** How the copy control reports each result until its normal label returns. */
const feedback = {
  idle: { icon: "copy", label: "common.copy" },
  written: { icon: "check", label: "common.copied" },
  requested: { icon: "copy", label: "common.copyRequested" },
  failed: { icon: "circle-alert", label: "common.copyFailed" },
} as const;

/** Copies `text` and briefly reports the result supplied by the platform. */
export function CopyButton(props: { text: string; label?: string; size?: ControlSize }) {
  const status = signal<ClipboardWrite | "idle">("idle");
  let timer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => clearTimeout(timer));
  return (
    <View class="flex-row items-center gap-1">
      <IconButton
        icon={feedback[status()].icon}
        label={status() === "idle" && props.label !== undefined ? props.label : t(feedback[status()].label)}
        size={props.size ?? "sm"}
        onPress={async () => {
          const result = await Clipboard.writeText(props.text).catch(() => "failed" as const);
          if (result === "failed") console.warn("The desktop clipboard write failed");
          status.set(result);
          clearTimeout(timer);
          timer = setTimeout(() => status.set("idle"), 1500);
        }}
      />
      {/* Wayland acknowledges the request; the action label already conveys this to screen readers. */}
      <If when={status() === "requested"}>
        <Text accessibilityElementsHidden class={`${inlineLabel} ${type.meta} text-muted-foreground`}>{t("common.copyRequested")}</Text>
      </If>
    </View>
  );
}
