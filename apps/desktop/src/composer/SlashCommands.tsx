import { computed, effect, keymap, List, signal, Text } from "nsh";
import type { InputRef, ListRef } from "nsh";
import { t } from "nsh/i18n";

import { commandQuery } from "../client/acp/commands";
import type { AvailableCommand } from "../client/acp/commands";
import type { SessionId } from "../client/acp/protocol";
import type { HostStore } from "../stores/host";
import { Textarea } from "../ui/input";
import { ListItem } from "../ui/list";
import { type } from "../ui/tokens";

/** The composer field and its Session's slash suggestions. A choice completes the draft without submitting it. */
export function SlashCommands(props: {
  host: HostStore;
  sessionId: SessionId | null;
  value: string;
  disabled: boolean;
  preparing: boolean;
  onPrepare?: () => Promise<void>;
  onChange: (text: string) => void;
  onSubmit: () => void;
  onEscape?: () => void;
}) {
  const query = computed(() => commandQuery(props.value));
  const matches = computed(() => {
    const prefix = query();
    if (prefix === undefined || props.sessionId === null) return [];
    return props.host.commandsFor(props.sessionId).filter((command) => command.name.toLowerCase().startsWith(prefix));
  });
  const dismissed = signal(false);
  const focused = signal(false);
  const index = signal(0);
  const current = () => Math.min(index(), Math.max(0, matches().length - 1));
  const showing = () => !props.disabled && props.sessionId !== null && query() !== undefined && !dismissed();
  let input: InputRef | undefined;
  let list: ListRef | undefined;
  let requested: typeof props.onPrepare;

  // The workspace owns its preparation callback. Keep at most one slash attempt per visit and prefix-editing session.
  effect(() => {
    if (query() === undefined) { requested = undefined; return; }
    const prepare = props.onPrepare;
    if (!prepare || requested === prepare || props.sessionId !== null || props.disabled || props.preparing) return;
    requested = prepare;
    void prepare().catch(() => { /* The empty composer reports the failure and keeps its draft. */ });
  });

  /** Finish the selected name and leave the insertion point ready for its arguments. */
  const choose = (command: AvailableCommand) => {
    const text = `/${command.name}${command.hint === undefined ? "" : " "}`;
    dismissed.set(true);
    props.onChange(text);
    input?.focus();
    input?.setSelection(text.length);
  };
  /** Accept a still-available selection or send the field normally once the chooser is closed. */
  const submit = () => {
    const choice = showing() ? matches()[current()] : undefined;
    if (choice) choose(choice);
    else props.onSubmit();
  };
  /** Keep the active row visible, including when cycling across a virtualized list of extension commands. */
  const move = (delta: number) => {
    const count = matches().length;
    if (!count) return;
    const next = (current() + delta + count) % count;
    index.set(next);
    list?.scrollToIndex({ index: next, animated: false, viewPosition: 0.5 });
  };
  // The textarea owns Enter, including IME confirmation and Shift+Enter. Bind only the palette's other keys.
  effect(() => {
    if (!focused()) return;
    if (!showing()) {
      const onEscape = props.onEscape;
      return onEscape && keymap.bind("escape", onEscape, { when: "always" });
    }
    const keys = [keymap.bind("escape", () => dismissed.set(true), { when: "always" })];
    if (matches().length) {
      keys.push(keymap.bind("down", () => move(1), { when: "always" }),
        keymap.bind("up", () => move(-1), { when: "always" }),
        keymap.bind("tab", submit, { when: "always" }));
    }
    return () => { for (const release of keys) release(); };
  });

  return (
    <>
      <If when={showing()}>
        {/* Show slash-command suggestions immediately without a fade-in transition. */}
        <If when={matches().length > 0} else={<Text class={`px-2 py-1 ${type.meta} text-muted-foreground`}>{t("commands.noMatches")}</Text>}>
          <List each={matches()} keyExtractor={(command) => command.name} estimatedItemSize={48}
            ref={(node) => { list = node; }} role="list" accessibilityLabel={t("commands.available")}
            style={{ height: Math.min(3, matches().length) * 48 }}>
            {(command, position) => (
              <ListItem label={`/${command().name}`} selected={current() === position()}
                description={[command().hint, command().description].filter(Boolean).join(" · ")}
                onPress={() => choose(command())} />
            )}
          </List>
        </If>
      </If>
      <Textarea variant="bare" value={props.value} inputRef={(node) => { input = node; }}
        placeholder={t("composer.placeholder")} accessibilityLabel={t("composer.label")}
        onChange={(value) => { index.set(0); dismissed.set(false); props.onChange(value); }}
        onFocus={() => focused.set(true)} onBlur={() => focused.set(false)} onSubmit={submit} />
    </>
  );
}
