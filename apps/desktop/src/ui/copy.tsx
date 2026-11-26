import { Clipboard, signal } from "nsh";
import { t } from "nsh/i18n";

import { IconButton } from "./button";
import type { ControlSize } from "./button";

/** Copies `text`; its glyph swaps to a check for 1.5 s to confirm. */
export function CopyButton(props: { text: string; label?: string; size?: ControlSize }) {
  const copied = signal(false);
  return (
    <IconButton
      icon={copied() ? "check" : "copy"}
      label={copied() ? t("common.copied") : props.label ?? t("common.copy")}
      size={props.size ?? "sm"}
      onPress={() => {
        void Clipboard.writeText(props.text).then(() => {
          copied.set(true);
          setTimeout(() => copied.set(false), 1500);
        });
      }}
    />
  );
}
