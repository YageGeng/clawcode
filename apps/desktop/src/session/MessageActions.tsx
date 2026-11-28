import { signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { MessageId } from "../client/acp/protocol";
import { IconButton } from "../ui/button";
import { CopyButton } from "../ui/copy";
import { Tooltip } from "../ui/menu";
import { inlineLabel, type } from "../ui/tokens";

/** Edit and Fork for a top-level Session's transcript; subagent Sessions have none. */
export type MessageEditing = Readonly<{
  /** The Session runs a turn: Edit and Fork wait for it. */
  busy: () => boolean;
  /** Puts user message `messageId` in the composer to ask it again. */
  edit: (messageId: MessageId) => void;
  /** Forks the Session up to assistant message `messageId`; settles once the new Session is open or failed. */
  fork: (messageId: MessageId) => Promise<void>;
  /** The user message the composer is editing, if any. */
  target: () => MessageId | undefined;
  /** Whether sending the edit drops this turn: every turn after the edited one. */
  dropped: (turnKey: string) => boolean;
}>;

/** Fork for a finished turn's reply: it spins while the host copies the history, and stays usable after a failure. */
function ForkButton(props: { messageId: MessageId; editing: MessageEditing }) {
  const forking = signal(false);
  const label = () => (props.editing.busy() ? t("edit.busy") : t("edit.fork"));
  return (
    <IconButton icon="git-fork" label={t("edit.fork")} size="sm" loading={forking()} disabled={props.editing.busy()}
      tooltip={<Tooltip text={label()} />}
      onPress={async () => {
        forking.set(true);
        // The host reports a failure as a notice; the button only stops spinning.
        await props.editing.fork(props.messageId).catch(() => undefined);
        forking.set(false);
      }} />
  );
}

/** What sits under an answer: copy it, fork from it (finished turns of a top-level Session), and when it was written. */
export function MessageActions(props: { messageId: MessageId; text: string; timestampMs: string; editing?: MessageEditing }) {
  const time = () => new Date(Number(props.timestampMs)).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  return (
    <View class="flex-row items-center gap-2">
      <View class="flex-row items-center gap-0.5">
        <CopyButton text={props.text} />
        <If when={props.editing}>{(editing) => <ForkButton messageId={props.messageId} editing={editing()} />}</If>
      </View>
      <Text class={`${inlineLabel} ${type.meta} text-muted-foreground`}>{time()}</Text>
    </View>
  );
}

/** Copy and Edit under a user message, shown on hover/focus or while that message is actively being edited. */
export function PromptActions(props: { messageId: MessageId; text: string; editing?: MessageEditing }) {
  const active = () => props.editing?.target() === props.messageId;
  return (
    <View class={`flex-row items-center gap-0.5 self-end ${active() ? "opacity-100" : "opacity-0 group-hover:opacity-100 group-focus-within:opacity-100"} transition-opacity duration-120 ease-out motion-reduce:transition-none`}>
      <CopyButton text={props.text} />
      <If when={props.editing}>
        {(editing) => (
          <IconButton icon="pencil" label={t("edit.edit")} size="sm" selected={active()} disabled={editing().busy()}
            tooltip={<Tooltip text={editing().busy() ? t("edit.busy") : t("edit.edit")} />}
            onPress={() => editing().edit(props.messageId)} />
        )}
      </If>
    </View>
  );
}
