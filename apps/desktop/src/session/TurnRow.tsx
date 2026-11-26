import { View } from "nsh";
import { t } from "nsh/i18n";

import type { MessageId, ToolCallId } from "../client/acp/protocol";
import type { MessageEntity, ToolCallEntity } from "../client/domain/model";
import type { TurnView, WorkStep } from "../client/turns";
import { MessageBubble } from "../ui/bubble";
import { Disclosure } from "../ui/disclosure";
import { Markdown } from "../ui/markdown";
import { ChangeCard } from "./ChangeCard";
import { MessageActions } from "./MessageActions";
import { ToolGroup } from "./ToolGroup";
import { motionClass } from "../ui/motion";

/** How long a turn took, as the interface language writes it ("12s", "3m 10s"). */
export function formatDuration(milliseconds: number): string {
  const total = Math.max(0, Math.round(milliseconds / 1000));
  return total < 60 ? t("time.seconds", { s: total }) : t("time.minutesSeconds", { m: Math.floor(total / 60), s: total % 60 });
}

/** What a turn row reads from its Session; entity lookups by id, so unchanged rows do not redraw. */
export type TurnSource = Readonly<{
  message: (id: MessageId) => MessageEntity | undefined;
  tool: (id: ToolCallId) => ToolCallEntity | undefined;
  /** Whether the user unfolded the part keyed `key` (a turn's work or a tool group). */
  isOpen: (key: string) => boolean;
  toggle: (key: string) => void;
}>;

/** A blinking caret after a streaming reply; still under reduced motion. */
function Caret() {
  return <View class={`h-4 w-1.5 rounded-full bg-foreground ${motionClass.pulse}`} />;
}

/** One turn: the user's pill, the work (folded once ended), the answer, changed files and actions. */
export function TurnRow(props: { turn: TurnView; source: TurnSource }) {
  const workKey = () => `work:${props.turn.key}`;
  const workOpen = () => props.turn.running || props.source.isOpen(workKey());
  const answer = () => (props.turn.answerMessageId === undefined ? undefined : props.source.message(props.turn.answerMessageId));
  const label = () =>
    props.turn.running ? t("turn.working") : t("turn.workedFor", { duration: formatDuration(props.turn.durationMs ?? 0) });
  return (
    <View class="w-full max-w-[760px] gap-4 self-center px-6 py-3">
      <If when={props.turn.userMessageId === undefined ? undefined : props.source.message(props.turn.userMessageId)}>
        {(message) => <MessageBubble text={message().text} arriving={props.turn.running} />}
      </If>
      <If when={props.turn.steps.length > 0}>
        <Disclosure label={label()} open={workOpen()} onToggle={() => props.source.toggle(workKey())} trailing={
            // `If` keeps one caret while the turn runs; a ternary here made a new one (restarting its pulse) per token.
            <If when={props.turn.running}>
              <Caret />
            </If>
          }
        >
          <View class="gap-3">
            <Each of={props.turn.steps} key={(step) => step.key}>
              {(step) => (
                // Read `step()` in bindings: a tool group gains calls while the turn runs.
                <View>
                  <If when={step().type === "message" ? (step() as Extract<WorkStep, { type: "message" }>).messageId : undefined}>
                    {(id) => <Markdown text={props.source.message(id())?.text ?? ""} streaming={props.turn.running} />}
                  </If>
                  <If when={step().type === "tools" ? (step() as Extract<WorkStep, { type: "tools" }>) : undefined}>
                    {(tools) => (
                      <ToolGroup
                        toolCallIds={tools().toolCallIds}
                        activities={tools().activities}
                        tool={props.source.tool}
                        open={props.source.isOpen(tools().key)}
                        onToggle={() => props.source.toggle(tools().key)}
                      />
                    )}
                  </If>
                </View>
              )}
            </Each>
          </View>
        </Disclosure>
      </If>
      {/* Changed files show while the turn runs and stay, in the same place, under the answer once it ends. */}
      <If when={answer() !== undefined || (props.turn.running && props.turn.changes.length > 0)}>
        <View class="gap-3">
          <If when={answer()}>{(message) => <Markdown text={message().text} />}</If>
          <Each of={props.turn.changes} key={(change) => change.path}>
            {(change) => <ChangeCard change={change()} />}
          </Each>
          <If when={answer()}>
            {(message) => <MessageActions text={message().text} timestampMs={message().endedAtMs} />}
          </If>
        </View>
      </If>
    </View>
  );
}
