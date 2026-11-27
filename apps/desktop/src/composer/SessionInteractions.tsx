import { computed, effect, Scroll, signal, Text, View } from "nsh";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import { initialInteractions, openQuestions, questionId } from "../client/workspace/interactions";
import type { SessionPlan, SessionQuestion } from "../client/workspace/interactions";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { Button, IconButton } from "../ui/button";
import { Disclosure } from "../ui/disclosure";
import { Textarea } from "../ui/input";
import { Markdown } from "../ui/markdown";
import { Checkbox, Select } from "../ui/selection";
import { Surface } from "../ui/surface";
import { type } from "../ui/tokens";

type QuestionDraft = Readonly<{ picks: readonly number[]; text: string }>;
const OTHER = -1;

/** The current plan and replaceable task list share one disclosure; long Markdown scrolls inside it. */
function PlanCard(props: { plan: SessionPlan; open: boolean; onToggle: () => void }) {
  /** Adds progress only when the host supplied tasks to count. */
  const title = () => props.plan.todos.length ? t("interactions.tasks", {
    done: props.plan.todos.filter((todo) => todo.status === "completed").length, total: props.plan.todos.length,
  }) : t("interactions.plan");
  return (
    <Surface>
      <Disclosure label={title()} icon="file-text" open={props.open} onToggle={props.onToggle}>
        <Scroll class="max-h-36" contentClass="gap-2 px-1 pb-1">
          <Each of={props.plan.todos}>
            {(todo) => (
              <View class="flex-row items-start gap-2">
                <Text class={`${type.meta} text-muted-foreground`}>{t(`interactions.${
                  todo().status === "completed" ? "completed" : todo().status === "in_progress" ? "active" : "pending"}`)}</Text>
                <Text class={`flex-1 ${type.ui} text-foreground`}>{todo().content}</Text>
              </View>
            )}
          </Each>
          <If when={props.plan.markdown}>
            {(markdown) => <View class="gap-1"><Text class={`${type.meta} text-muted-foreground`}>{t("interactions.approved")}</Text><Markdown text={markdown()} /></View>}
          </If>
        </Scroll>
      </Disclosure>
    </Surface>
  );
}

/** Edits one stable question without consuming the ordinary composer draft or stealing keyboard focus. */
function QuestionEditor(props: {
  host: HostStore; sessionId: SessionId; question: SessionQuestion; questions: readonly SessionQuestion[];
  index: number; move: (index: number) => void; drafts: Map<string, QuestionDraft>; draftId: string;
}) {
  const initial = props.drafts.get(props.draftId) ?? { text: "", picks: [props.question.options.length ? 0 : OTHER] };
  const draft = signal<QuestionDraft>(initial);
  const error = signal("");
  /** Only a submission for this question disables its editor. */
  const pending = () => props.host.questionPending(props.sessionId, props.question.key);
  /** Offline Sessions keep the current question readable. */
  const disabled = () => props.host.phase().kind !== "ready" || pending();
  const menu = computed(() => [...props.question.options.map((title, index) => ({ value: String(index), title })),
    { value: String(OTHER), title: t("interactions.other") }]);
  /** Sends one text block, preserving option order and the kernel's Markdown list format for multiple picks. */
  const answer = () => {
    const other = draft().picks.includes(OTHER) && draft().text.trim() ? [draft().text] : [];
    const selected = [...props.question.options.filter((_, index) => draft().picks.includes(index)), ...other];
    return props.question.multiSelect ? selected.map((item) => `- ${item}`).join("\n") : selected[0] ?? "";
  };
  /** Keeps each draft with its host, Session and wire question as the user pages through a batch. */
  const edit = (next: QuestionDraft) => {
    draft.set(next);
    props.drafts.set(props.draftId, next);
    error.set("");
  };
  /** Applies a radio selection or toggles one checkbox, retaining the free text for later re-selection. */
  const choose = (index: number) => {
    const picks = draft().picks;
    edit({ ...draft(), picks: !props.question.multiSelect ? [index] : picks.includes(index) ? picks.filter((pick) => pick !== index) : [...picks, index] });
  };
  /** Settles the captured question(s); a rejection leaves its draft available for retry. */
  const submit = async (operation: "answer" | "skip" | "skipAll") => {
    if (disabled()) return;
    const host = props.host;
    const session = props.sessionId;
    const key = props.question.key;
    const draftId = props.draftId;
    const targets = operation === "skipAll" ? props.questions.map((item) => item.key) : [key];
    try {
      if (operation === "answer") await host.answerQuestion(session, key, answer());
      else await host.dismissQuestions(session, targets);
      props.drafts.delete(draftId);
    } catch {
      error.set(t("interactions.failed"));
    }
  };
  return (
    <Appear entrance="rise">
      <Surface>
        <View class="flex-row items-center gap-2">
          <Text class={`flex-1 ${type.meta} font-semibold text-muted-foreground`}>{t("interactions.question", { current: props.index + 1, total: props.questions.length })}</Text>
          <If when={props.questions.length > 1}>
            <IconButton icon="arrow-left" size="sm" label={t("interactions.previous")} disabled={props.index === 0} onPress={() => props.move(props.index - 1)} />
            <IconButton icon="arrow-right" size="sm" label={t("interactions.next")} disabled={props.index + 1 === props.questions.length} onPress={() => props.move(props.index + 1)} />
          </If>
        </View>
        <Scroll class="max-h-40" contentClass="gap-2 pb-1">
          <Markdown text={props.question.title} />
          <If when={props.question.options.length > 0}>
            <If when={props.question.multiSelect} else={
              <Select size="sm" items={menu()} value={String(draft().picks[0] ?? OTHER)} disabled={disabled()} accessibilityLabel={t("interactions.choice")}
                onChange={(value) => { if (value !== null) choose(Number(value)); }} />
            }>
              <Each of={menu()} key={(item) => item.value}>
                {(item) => <Checkbox label={item().title} checked={draft().picks.includes(Number(item().value))}
                  disabled={disabled()} onChange={() => choose(Number(item().value))} />}
              </Each>
            </If>
          </If>
          <If when={draft().picks.includes(OTHER)}>
            <Textarea value={draft().text} minLines={1} maxLines={3} disabled={disabled()} accessibilityLabel={t("interactions.freeText")}
              placeholder={t("interactions.freeText")} onChange={(text) => edit({ ...draft(), text })} />
          </If>
        </Scroll>
        <If when={error()}>{(reason) => <Text role="alert" class={`${type.meta} text-destructive`}>{reason()}</Text>}</If>
        <View class="flex-row items-center justify-end gap-2">
          <If when={props.questions.length > 1}>
            <Button label={t("interactions.skipAll")} size="sm" variant="ghost"
              disabled={disabled() || props.questions.some((item) => props.host.questionPending(props.sessionId, item.key))}
              onPress={() => void submit("skipAll")} />
          </If>
          <Button label={t("interactions.skip")} size="sm" variant="ghost" disabled={disabled()} onPress={() => void submit("skip")} />
          <Button label={t("interactions.send")} size="sm" variant="primary" loading={pending()}
            disabled={disabled() || !answer().trim()} onPress={() => void submit("answer")} />
        </View>
      </Surface>
    </Appear>
  );
}

/** The current host's open questions and plan, above the composer and below a pending permission in priority. */
export function SessionInteractions(props: { host: HostStore; sessionId: SessionId | null }) {
  const data = computed(() => props.sessionId ? props.host.session(props.sessionId)?.interactions ?? initialInteractions : initialInteractions);
  // Independent selectors keep token updates and queue changes from rebuilding unrelated rows.
  const questions = computed(() => openQuestions(data()));
  const plan = computed(() => data().plan);
  const planIdentity = computed(() => [props.host.info.id, props.sessionId, plan().markdown] as const,
    { equals: (left, right) => left.every((value, index) => value === right[index]) });
  const planOpen = signal(false);
  // A new plan starts closed. Run transitions and task progress preserve the reader's choice.
  effect(() => { planIdentity(); planOpen.set(false); });
  const drafts = new Map<string, QuestionDraft>();
  const position = signal<{ host: string; session: SessionId | null; id: string; index: number }>();
  const index = computed(() => {
    const selected = position();
    if (!selected || selected.host !== props.host.info.id || selected.session !== props.sessionId) return 0;
    const items = questions();
    const match = items.findIndex((item) => questionId(item.key) === selected.id);
    return match >= 0 ? match : Math.max(0, Math.min(selected.index, items.length - 1));
  });
  /** Anchor navigation to a question; its last position selects a neighbor only when that question closes. */
  const select = (index: number) => {
    const item = questions()[index];
    const previous = position();
    if (!item) {
      if (previous) position.set(undefined);
      return;
    }
    const id = questionId(item.key);
    const host = props.host.info.id, session = props.sessionId;
    if (previous?.id !== id || previous.host !== host || previous.session !== session || previous.index !== index) {
      position.set({ host, session, id, index });
    }
  };
  // Record automatic selections too, so a reservation released on another client cannot displace the current editor.
  effect(() => select(index()));
  const current = computed(() => questions().slice(index(), index() + 1));
  /** A question's wire key is only unique inside its owning Session and host. */
  const draftId = (question: SessionQuestion) => JSON.stringify([props.host.info.id, props.sessionId, questionId(question.key)]);
  return (
    <If when={props.sessionId && !props.host.permissionFor(props.sessionId) && (questions().length || plan().markdown || plan().todos.length)}>
      <View class="gap-2">
        <If when={plan().markdown || plan().todos.length}>
          <PlanCard plan={plan()} open={planOpen()} onToggle={() => planOpen.update((open) => !open)} />
        </If>
        <Each of={current()} key={draftId}>
          {(question) => <QuestionEditor host={props.host} sessionId={props.sessionId!} question={question()} questions={questions()}
            index={index()} move={select}
            drafts={drafts} draftId={draftId(question())} />}
        </Each>
      </View>
    </If>
  );
}
