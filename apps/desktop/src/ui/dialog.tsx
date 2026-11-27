import { computed, Text, View } from "nsh";
import { t } from "nsh/i18n";
import { AlertDialog, Dialog as BaseDialog, Sheet as BaseSheet } from "nsh/base";
import type { JSX } from "nsh/jsx-runtime";

import { IconButton } from "./button";
import { inlineLabel } from "./tokens";

/** The title row of a dialog or a sheet, with its close button. */
function PanelHeader(props: { title: string; onClose: () => void }) {
  return (
    <View class="flex-row items-center gap-2">
      {/* Device names in confirmation titles may wrap; reserve the close button's hit area. */}
      <Text class={`flex-1 ${inlineLabel} text-base leading-6 font-semibold text-popover-foreground`}>{props.title}</Text>
      <IconButton icon="x" label={t("common.close")} size="sm" onPress={props.onClose} />
    </View>
  );
}

/** A centered dialog with a title, its content and its actions; Escape and the close button dismiss it. */
export function Dialog(props: {
  open: boolean;
  title: string;
  children: JSX.Element;
  actions?: JSX.Element;
  /** Let the owner switch back to a parent view or asynchronously reject an access request before closing. */
  controlledDismissal?: boolean;
  onClose: () => void;
}) {
  // A JSX slot is a getter that builds a new element on every read, `If`'s accessor included; read it once.
  const actions = computed(() => props.actions);
  return (
    <BaseDialog
      open={props.open}
      animation="fade"
      accessibilityLabel={props.title}
      closeOnEscape={!props.controlledDismissal}
      closeOnBackdropPress={!props.controlledDismissal}
      closeOnSwipe={!props.controlledDismissal}
      onCancel={() => { if (props.controlledDismissal) props.onClose(); }}
      onOpenChange={(event) => {
        if (!event.open) props.onClose();
      }}
      backdropClass="bg-foreground/30"
      class="m-auto w-[520px] max-w-[92%] gap-4 rounded-(--radius-lg) border border-border bg-popover p-6 shadow-lg"
    >
      <PanelHeader title={props.title} onClose={props.onClose} />
      {props.children}
      <If when={actions()}>
        <View class="flex-row justify-end gap-2">{actions()}</View>
      </If>
    </BaseDialog>
  );
}

/** What `confirm` asks: the question names the object, the action names the verb (`Delete "Roadmap"?`, `Delete`). */
export type ConfirmOptions = Readonly<{ title: string; description?: string; action: string; danger?: boolean }>;

/** Asks the user to confirm with the system's alert dialog; resolves true when they chose the action. */
export async function confirm(options: ConfirmOptions): Promise<boolean> {
  const result = await AlertDialog.show({
    title: options.title,
    description: options.description ?? null,
    closeOnEscape: true,
    actions: [
      { id: "cancel", label: t("common.cancel"), role: "cancel" },
      { id: "confirm", label: options.action, role: options.danger === true ? "destructive" : "default" },
    ],
  });
  return result.action === "confirm";
}

/** A panel sliding in from a window edge, for secondary content. */
export function Sheet(props: { open: boolean; title: string; side?: "left" | "right"; children: JSX.Element; onClose: () => void }) {
  return (
    <BaseSheet
      open={props.open}
      side={props.side ?? "right"}
      accessibilityLabel={props.title}
      onOpenChange={(event) => {
        if (!event.open) props.onClose();
      }}
      backdropClass="bg-foreground/20"
      popupClass="w-[360px] gap-4 border-l border-border bg-popover p-5 shadow-lg"
    >
      <PanelHeader title={props.title} onClose={props.onClose} />
      {props.children}
    </BaseSheet>
  );
}
