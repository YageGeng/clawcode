import { computed, untracked, View } from "nsh";
import { t } from "nsh/i18n";

import type { MessageId, ToolCallId } from "../client/acp/protocol";
import type { FileChange, MessageEntity, TerminalEntity, ToolCallEntity } from "../client/domain/model";
import type { TurnView, WorkStep } from "../client/turns";
import { MessageBubble } from "../ui/bubble";
import { Disclosure } from "../ui/disclosure";
import { Markdown } from "../ui/markdown";
import { motionClass } from "../ui/motion";
import { ChangeCard } from "./ChangeCard";
import { MessageActions } from "./MessageActions";
import type { ChangeOpener } from "./SessionView";
import { AssistantContent } from "./ToolContent";
import { ToolRow } from "./ToolRow";

/** How long a turn took, as the interface language writes it ("12s", "3m 10s"). */
export function formatDuration(milliseconds: number): string {
  const total = Math.max(0, Math.round(milliseconds / 1000));
  return total < 60 ? t("time.seconds", { s: total }) : t("time.minutesSeconds", { m: Math.floor(total / 60), s: total % 60 });
}

/** Session-scoped entities and the disclosure choices shared by recycled transcript cells. */
export type TurnSource = Readonly<{
  message: (id: MessageId) => MessageEntity | undefined;
  tool: (id: ToolCallId) => ToolCallEntity | undefined;
  terminal: (id: string) => TerminalEntity | undefined;
  isOpen: (key: string) => boolean;
  toggle: (key: string) => void;
}>;

/** A blinking caret on the working header; still under reduced motion. */
function Caret() {
  return <View class={`h-4 w-1.5 rounded-full bg-foreground ${motionClass.pulse}`} />;
}

/** Renders message content in either the reply or an opened commentary row. */
function AssistantMessage(props: { message?: MessageEntity; running: boolean; showActions?: boolean }) {
  const media = computed(() => props.message?.attachments);
  // Text before a media block is complete. Only a new media layout or authoritative snapshot can change it.
  const completed = computed(() => {
    const attachments = media();
    const text = untracked(() => props.message?.text ?? "");
    return attachments?.map(({ textOffset, block }, index) =>
      ({ text: text.slice(attachments[index - 1]?.textOffset ?? 0, textOffset), block }));
  });
  return (
    <If when={props.message}>
      {(message) => (
        <View class="gap-2">
          <Each of={completed()} key="index">
            {(part) => <View class="gap-2">
              <If when={part().text}>{(text) => <Markdown text={text()} />}</If>
              <AssistantContent blocks={[part().block]} />
            </View>}
          </Each>
          <If when={message().text.slice(media()?.at(-1)?.textOffset ?? 0)}>
            {(text) => <Markdown text={text()} streaming={props.running} />}
          </If>
          <If when={props.showActions && !!message().text}>
            <MessageActions text={message().text} timestampMs={message().endedAtMs} />
          </If>
        </View>
      )}
    </If>
  );
}

/** Shows a user prompt or each recorded question and choice at the position of its durable message. */
function UserMessage(props: { message: MessageEntity; arriving?: boolean }) {
  return (
    <If when={props.message.decisions} else={
      <MessageBubble text={props.message.text} imageCount={props.message.imageCount} arriving={props.arriving} />
    }>
      {(items) => <Each of={items()} key={(item) => item.id}>
        {(item) => <MessageBubble context={item().question} text={item().answer ?? t("turn.skippedQuestion")} arriving={false} />}
      </Each>}
    </If>
  );
}

/** Discloses the individual steps of a contiguous stretch of work. */
function WorkRows(props: { steps: readonly WorkStep[]; source: TurnSource; running: boolean }) {
  return (
    <View class="ml-2.5 gap-1.5 border-l border-border pl-3">
      <Each of={props.steps} key={(step) => step.key}>
        {(step) => {
          // The keyed row's kind and entity identity stay fixed across streamed updates.
          const item = step();
          if (item.type === "tool") return <ToolRow id={item.toolCallId} source={props.source} />;
          if (item.type === "message") return (
            <Disclosure label={t("turn.message")} open={props.source.isOpen(item.key)} onToggle={() => props.source.toggle(item.key)}>
              <AssistantMessage message={props.source.message(item.messageId)} running={false} />
            </Disclosure>
          );
          return (
            <Disclosure label={t("turn.thinking")} open={props.source.isOpen(item.key)} onToggle={() => props.source.toggle(item.key)}>
              <Markdown text={props.source.message(item.messageId)?.reasoning ?? ""} tone="muted" streaming={props.running} />
            </Disclosure>
          );
        }}
      </Each>
    </View>
  );
}

/** Shows the prompt and reply directly, with separate disclosures for work and changed files. */
export function TurnRow(props: {
  turn: TurnView;
  source: TurnSource;
  selectedPath?: string;
  onViewChange: (turnKey: string, file: FileChange, trigger?: ChangeOpener) => void;
}) {
  const changesKey = () => `files:${props.turn.key}`;
  const lastWork = computed(() => props.turn.sections.findLast((section) => section.type === "work")?.key);
  const label = () => props.turn.running ? t("turn.working") : props.turn.durationMs === undefined
    ? t("turn.work") : t("turn.workedFor", { duration: formatDuration(props.turn.durationMs) });
  return (
    <View class="w-full max-w-[760px] gap-3 self-center px-6 py-3">
      <If when={props.turn.userMessageId === undefined ? undefined : props.source.message(props.turn.userMessageId)}>
        {(message) => <UserMessage message={message()} arriving={props.turn.running} />}
      </If>
      <Each of={props.turn.sections} key={(section) => section.key}>
        {(section) => {
          const item = section();
          if (item.type === "decision") return (
            <If when={props.source.message(item.messageId)}>{(message) => <UserMessage message={message()} />}</If>
          );
          if (item.type === "answer") return (
            <AssistantMessage message={props.source.message(item.messageId)} running={props.turn.running} showActions={!props.turn.running} />
          );
          // A work section retains its key as more steps arrive, so read its steps through the row accessor.
          const steps = () => {
            const current = section();
            return current.type === "work" ? current.steps : [];
          };
          return (
            <Disclosure flush label={lastWork() === item.key ? label() : t("turn.work")} open={props.source.isOpen(item.key)}
              onToggle={() => props.source.toggle(item.key)}
              trailing={<If when={props.turn.running && lastWork() === item.key}><Caret /></If>}>
              <WorkRows steps={steps()} source={props.source} running={props.turn.running} />
            </Disclosure>
          );
        }}
      </Each>
      <If when={props.turn.changes.length > 0}>
        <Disclosure flush label={t("turn.changedFiles", { count: props.turn.changes.length })} open={props.source.isOpen(changesKey())}
          onToggle={() => props.source.toggle(changesKey())}>
          <Each of={props.turn.changes} key={(change) => change.path}>
            {(change) => (
              <ChangeCard
                change={change()}
                selected={props.selectedPath === change().path}
                onOpen={(trigger) => props.onViewChange(props.turn.key, change(), trigger)}
              />
            )}
          </Each>
        </Disclosure>
      </If>
    </View>
  );
}
