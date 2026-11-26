import { cleanup, Overlay, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";
import type { Signal } from "nsh";
import { Toast as BaseToast, ToastManager, ToastStack } from "nsh/base";
import type { ToastEntry } from "nsh/base";

import { IconButton } from "./button";
import { Icon } from "./icon";
import type { IconName } from "./icon";
import { motion, motionClass } from "./motion";
import type { Tone } from "./tokens";
import { toneToken, type } from "./tokens";

/** What a toast says. */
export type ToastMessage = Readonly<{ title: string; description?: string; tone?: Tone }>;

/** The icon of each tone, so a toast never carries meaning by color alone. */
const toneIcon: Readonly<Record<Tone, IconName>> = {
  neutral: "info",
  success: "circle-check",
  warning: "triangle-alert",
  danger: "circle-alert",
  info: "info",
};

/** The app's toasts: one manager, read by `Toasts` and written by `show` and `dismiss` only. */
export class ToastStore {
  private readonly manager = new ToastManager({ duration: motion.slow, exitDuration: motion.base });
  private readonly entries: Signal<readonly ToastEntry[]> = signal<readonly ToastEntry[]>([]);
  private sequence = 0;

  /** The toasts to draw, oldest first. */
  visible(): readonly ToastEntry[] {
    return this.entries();
  }

  /** Shows a toast for `timeoutMs` (4 s by default); returns its id. */
  show(message: ToastMessage, timeoutMs = 4000): string {
    const id = `toast-${++this.sequence}`;
    this.manager.push(id, message, { timeout: timeoutMs });
    this.sync();
    return id;
  }

  /** Begins a toast's exit. */
  dismiss(id: string): void {
    if (this.manager.dismiss(id)) this.sync();
  }

  /** Moves the toasts' lifecycle to now; the stack calls it on a timer while any is mounted. */
  advance(paused: boolean): void {
    if (this.manager.advance(paused).changed) this.sync();
  }

  /** Copies the manager's mounted toasts into the signal the stack reads. */
  private sync(): void {
    this.entries.set(this.manager.visible(3));
  }
}

/** The stack of toasts at the window's bottom-right edge. */
export function Toasts(props: { store: ToastStore }) {
  const expanded = signal(false);
  // ponytail: a fixed 100 ms tick while mounted; drive it from the manager's next deadline if it shows in traces.
  const timer = setInterval(() => props.store.advance(expanded()), 100);
  cleanup(() => clearInterval(timer));
  return (
    <Overlay class="absolute bottom-4 right-4 w-80">
      <ToastStack
        placement="bottomRight"
        onExpandedChange={(event) => expanded.set(event.expanded)}
        onCancel={() => {
          const newest = props.store.visible().at(-1);
          if (newest !== undefined) props.store.dismiss(newest.id);
        }}
      >
        <Each of={[...props.store.visible()]} key={(entry) => entry.id}>
          {(entry) => <ToastCard entry={entry()} onDismiss={() => props.store.dismiss(entry().id)} />}
        </Each>
      </ToastStack>
    </Overlay>
  );
}

/** One toast: tone icon, title, description and a close button. */
function ToastCard(props: { entry: ToastEntry; onDismiss: () => void }) {
  const message = () => props.entry.value as ToastMessage;
  const tone = () => message().tone ?? "neutral";
  return (
    <BaseToast
      transitionStatus={props.entry.status}
      accessibilityLabel={message().title}
      onDismissRequest={props.onDismiss}
      swipeDirection="right"
      class={`flex-row items-start gap-2 rounded-(--radius-lg) border border-border bg-popover p-3 shadow-lg ${motionClass.toast}`}
      startingClass={motionClass.toastStarting}
      endingClass={motionClass.toastEnding}
    >
      <Icon name={toneIcon[tone()]} color={toneToken[tone()]} />
      <View class="flex-1 gap-0.5">
        <Text class={`${type.ui} font-medium text-popover-foreground`}>{message().title}</Text>
        <If when={message().description}>{(text) => <Text class={`${type.meta} text-muted-foreground`}>{text()}</Text>}</If>
      </View>
      <IconButton icon="x" label={t("common.dismiss")} size="sm" onPress={props.onDismiss} />
    </BaseToast>
  );
}
