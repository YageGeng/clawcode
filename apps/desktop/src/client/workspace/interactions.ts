import { AcpProtocol } from "../acp/protocol";
import type { QuestionDecision } from "../domain/model";

/** The identity ACP expects for one asynchronous question. */
export type QuestionKey = Readonly<{ requestId: string; index: number }>;
export type SessionQuestion = Readonly<{ key: QuestionKey; title: string; options: readonly string[]; multiSelect: boolean }>;
export type PlanTodo = Readonly<{ content: string; status: string }>;
export type SessionPlan = Readonly<{ markdown?: string; todos: readonly PlanTodo[] }>;
export type InteractionState = Readonly<{
  questions: readonly SessionQuestion[];
  settled: ReadonlySet<string>;
  queued: ReadonlySet<string>;
  plan: SessionPlan;
}>;

export const initialInteractions: InteractionState = { questions: [], settled: new Set(), queued: new Set(), plan: { todos: [] } };

export type InteractionChange =
  | { type: "asked"; questions: readonly SessionQuestion[] }
  | { type: "settled" | "queued"; ids: readonly string[] }
  | { type: "todos"; todos: readonly PlanTodo[] }
  | { type: "plan"; markdown: string };

/** A key for host-local question projections; the numeric index is always the last segment. */
export function questionId(key: QuestionKey): string {
  return `${key.requestId}:${key.index}`;
}

/** Validates every identity before accepting a complete snapshot or durable settlement. */
function questionIds(value: unknown, field: "requestId" | "request_id"): string[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const ids: string[] = [];
  for (const entry of value) {
    const item = AcpProtocol.record(entry);
    const requestId = item?.[field];
    if (typeof requestId !== "string" || !requestId || typeof item?.index !== "number" || !Number.isSafeInteger(item.index) || item.index < 0) {
      return undefined;
    }
    ids.push(questionId({ requestId, index: item.index }));
  }
  return ids;
}

/** Decodes settled choices once for both the visible transcript and the question ledger. */
export function questionDecisions(message: unknown, namespace: string): readonly QuestionDecision[] | undefined {
  const content = AcpProtocol.record(AcpProtocol.record(message)?.content);
  if (typeof content?.type !== "string") return undefined;
  if (content.type !== "extension") return [];
  const extension = AcpProtocol.record(content.extension);
  if (typeof extension?.extension_id !== "string" || typeof extension.custom_type !== "string") return undefined;
  if (extension.extension_id !== namespace || extension.custom_type !== "question_reply") return [];
  const items = AcpProtocol.record(extension.details)?.items;
  const ids = questionIds(items, "request_id");
  if (!ids || !Array.isArray(items)) return undefined;
  return items.map((item, index) => ({
    id: ids[index]!, question: typeof item.question === "string" ? item.question : "",
    ...(typeof item.answer === "string" ? { answer: item.answer } : {}),
  }));
}

/** A full snapshot replaces temporary reservations, including when an unwritten answer is returned. */
export function queuedQuestions(value: unknown, namespace: string): InteractionChange | undefined {
  const queue = AcpProtocol.record(value);
  if (queue && "reservedQuestions" in queue) {
    const ids = questionIds(queue.reservedQuestions, "requestId");
    return ids ? { type: "queued", ids } : undefined;
  }
  // Older hosts only expose removable answers in their two queue lanes.
  if (!Array.isArray(queue?.steering) || !Array.isArray(queue.followUp)) return undefined;
  const ids: string[] = [];
  for (const lane of [queue.steering, queue.followUp]) {
    for (const entry of lane) {
      const decisions = questionDecisions(AcpProtocol.record(entry)?.message, namespace);
      if (!decisions) return undefined;
      for (const decision of decisions) ids.push(decision.id);
    }
  }
  return { type: "queued", ids };
}

/** Decodes persisted asks, plans and queue reservations alongside ordinary transcript updates. */
export function interactionUpdate(update: Readonly<Record<string, unknown>>, namespace: string): InteractionChange | undefined {
  if (update.sessionUpdate === "tool_call_update") {
    // The live tool-end frame has rawOutput.details before the kernel accepts replies. Only its persisted message opens the card.
    const content = AcpProtocol.record(AcpProtocol.record(update.rawOutput)?.content);
    const details = AcpProtocol.record(content?.details);
    if (typeof update.toolCallId !== "string" || details?.type !== "user_questions"
      || typeof details.request_id !== "string" || !Array.isArray(details.questions)) return undefined;
    const requestId = details.request_id;
    const questions = details.questions.flatMap((item: unknown, index: number): SessionQuestion[] => {
      const record = AcpProtocol.record(item);
      if (typeof record?.title !== "string") return [];
      const options = Array.isArray(record.options) ? record.options.filter((value): value is string => typeof value === "string") : [];
      // Keep wire indices even when an earlier malformed question is omitted.
      return [{ key: { requestId, index }, title: record.title, options, multiSelect: record.multi_select === true }];
    });
    return { type: "asked", questions };
  }
  if (update.sessionUpdate === "plan_update") {
    const plan = AcpProtocol.record(update.plan);
    if (plan?.planId === "plan-mode" && plan.type === "markdown" && typeof plan.content === "string") return { type: "plan", markdown: plan.content };
    if (plan?.planId === "todos" && plan.type === "items" && Array.isArray(plan.entries)) {
      const todos = plan.entries.flatMap((entry: unknown) => {
        const item = AcpProtocol.record(entry);
        return typeof item?.content === "string" && typeof item.status === "string" ? [{ content: item.content, status: item.status }] : [];
      });
      return { type: "todos", todos };
    }
  }
  if (update.sessionUpdate === `_${namespace}/event`) {
    const event = AcpProtocol.record(update.payload);
    if (event?.event === "queue_changed") return queuedQuestions(event.pending, namespace);
  }
  return undefined;
}

/** Owns the question ledger for one live frame or one complete history replay. */
export class InteractionDraft {
  private settled?: Set<string>;

  /** Starts by sharing the last published state until an event changes it. */
  constructor(public state: InteractionState) {}

  /** Applies events in wire order, taking ownership of the settled IDs on the first new reply. */
  apply(change: InteractionChange): void {
    const state = this.state;
    switch (change.type) {
      case "asked": {
        const known = new Set(state.questions.map((question) => questionId(question.key)));
        const added = change.questions.filter(({ key }) => !known.has(questionId(key)) && !state.settled.has(questionId(key)));
        if (added.length) this.state = { ...state, questions: [...state.questions, ...added] };
        break;
      }
      case "settled": {
        if (change.ids.every((id) => state.settled.has(id))) break;
        const settled = this.settled ??= new Set(state.settled);
        for (const id of change.ids) settled.add(id);
        this.state = { ...state, settled,
          questions: state.questions.filter(({ key }) => !settled.has(questionId(key))) };
        break;
      }
      case "queued": {
        const queued = new Set(change.ids);
        if (queued.size !== state.queued.size || change.ids.some((id) => !state.queued.has(id))) {
          this.state = { ...state, queued };
        }
        break;
      }
      case "todos": this.state = { ...state, plan: { markdown: state.plan.markdown, todos: change.todos } }; break;
      case "plan": this.state = { ...state, plan: { todos: state.plan.todos, markdown: change.markdown } }; break;
    }
  }

  /** Releases ownership so another event cannot mutate a published set. */
  commit(): InteractionState {
    this.settled = undefined;
    return this.state;
  }
}

/** Returns open questions in asking order, suppressing answers queued here or on another client. */
export function openQuestions(state: InteractionState): readonly SessionQuestion[] {
  return state.queued.size ? state.questions.filter(({ key }) => !state.queued.has(questionId(key))) : state.questions;
}
