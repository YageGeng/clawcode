import type { MessageId, ToolCallId } from "./acp/protocol";
import type { FileChange, RecordedFileChange } from "./domain/model";
import type { SessionWorkspaceState } from "./workspace/sessionState";

/** One item inside a turn's folded work; `key` is stable while the turn grows. */
export type WorkStep =
  | Readonly<{ type: "thought" | "message"; key: string; messageId: MessageId }>
  | Readonly<{ type: "tool"; key: string; toolCallId: ToolCallId }>;

/** Decisions stay visible between the chronological stretches of folded work. */
export type TurnSection =
  | Readonly<{ type: "work"; key: string; steps: readonly WorkStep[] }>
  | Readonly<{ type: "decision" | "answer"; key: string; messageId: MessageId }>;

/** A user message and everything the agent did for it. */
export type TurnView = Readonly<{
  /** The user message id, or the first entry's key for a turn without one. */
  key: string;
  userMessageId?: MessageId;
  /** Ordered work disclosures, user decisions and the directly visible final or streaming reply. */
  sections: readonly TurnSection[];
  /** Changed files merged per path, in first-seen order. */
  changes: readonly FileChange[];
  running: boolean;
  /** The last entry's end minus the user message's start, once the turn ended. */
  durationMs?: number;
}>;

/** A turn being collected: entries in order, plus the user message that opened it. */
type Collecting = { userMessageId?: MessageId; entries: Array<Readonly<{ type: "message"; id: MessageId } | { type: "tool"; id: ToolCallId }>> };

/** Groups one mounted Session's transcript, retaining completed rows across live appends. */
export class TurnGroups {
  private layout?: SessionWorkspaceState["turnLayout"];
  private running = false;
  private rows: readonly TurnView[] = [];
  private lastStart = 0;

  /** Rebuilds the tail on structural changes and the full transcript on replay or an earlier correction. */
  update(session: SessionWorkspaceState): readonly TurnView[] {
    if (session.turnLayout === this.layout && session.running === this.running) return this.rows;
    const keepHistory = this.layout?.history === session.turnLayout.history && this.rows.length > 0;
    const prefix = keepHistory ? this.rows.length - 1 : 0;
    const start = keepHistory ? this.lastStart : 0;
    const collected: Collecting[] = [];
    this.lastStart = start;
    for (let i = start; i < session.transcript.length; i++) {
      const entry = session.transcript[i]!;
      if (entry.type === "message") {
        const message = session.messages.get(entry.messageId);
        if (!message) continue;
        if (message.role === "user" && !message.continuesTurn) {
          this.lastStart = i;
          collected.push({ userMessageId: message.messageId, entries: [] });
          continue;
        }
      }
      if (!collected.length) collected.push({ entries: [] });
      collected.at(-1)!.entries.push(entry.type === "message"
        ? { type: "message", id: entry.messageId } : { type: "tool", id: entry.toolCallId });
    }
    const tail = collected.map((turn, i) => {
      const next = foldTurn(session, turn, prefix + i, i + 1 === collected.length);
      const previous = this.rows[prefix + i];
      return previous && previous.key === next.key && sameTurn(previous, next) ? previous : next;
    });
    if (prefix + tail.length !== this.rows.length || tail.some((row, i) => row !== this.rows[prefix + i])) {
      this.rows = prefix ? [...this.rows.slice(0, prefix), ...tail] : tail;
    }
    this.layout = session.turnLayout;
    this.running = session.running;
    return this.rows;
  }
}

/** One collected turn as the transcript shows it; only the newest turn can still be running. */
function foldTurn(session: SessionWorkspaceState, turn: Collecting, index: number, newest: boolean): TurnView {
  const running = session.running && newest;
  // A skip written as the run closes must not reclassify the final assistant reply as commentary.
  const tail = turn.entries.findLast((item) => item.type === "tool" || !session.messages.get(item.id)?.decisions);
  const sections: TurnSection[] = [];
  const first = turn.entries[0];
  const key = turn.userMessageId ?? (first === undefined ? `turn-${index}` : `${first.type}:${first.id}`);
  let workKey = `work:${key}`;
  let steps: WorkStep[] = [];
  /** Flushes one contiguous stretch of work before a visible answer or decision. */
  const flushWork = () => {
    if (steps.length) sections.push({ type: "work", key: workKey, steps });
    steps = [];
  };
  const changes = new Map<string, { added: number; removed: number; edits: [RecordedFileChange, ...RecordedFileChange[]] }>();
  for (const item of turn.entries) {
    if (item.type === "message") {
      const message = session.messages.get(item.id);
      if (message?.decisions) {
        flushWork();
        sections.push({ type: "decision", key: `decision:${item.id}`, messageId: item.id });
        workKey = `work:${item.id}`;
        continue;
      }
      if (message?.reasoning) steps.push({ type: "thought", key: `thought:${item.id}`, messageId: item.id });
      if (message?.text || message?.attachments?.length) {
        if (item === tail) {
          flushWork();
          sections.push({ type: "answer", key: `answer:${item.id}`, messageId: item.id });
        } else steps.push({ type: "message", key: `message:${item.id}`, messageId: item.id });
      }
      continue;
    }
    const tool = session.tools.get(item.id);
    steps.push({ type: "tool", key: `tool:${item.id}`, toolCallId: item.id });
    for (const change of tool?.changes ?? []) {
      const existing = changes.get(change.path);
      if (existing) {
        existing.added += change.added;
        existing.removed += change.removed;
        existing.edits.push(change);
      } else {
        changes.set(change.path, { added: change.added, removed: change.removed, edits: [change] });
      }
    }
  }
  flushWork();
  return {
    key,
    ...(turn.userMessageId === undefined ? {} : { userMessageId: turn.userMessageId }),
    sections,
    changes: [...changes].map(([path, counts]) => ({ path, ...counts })),
    running,
    ...(running ? {} : durationOf(session, turn)),
  };
}

/** Measures a finished turn from its user message to the latest timestamp in its entries. */
function durationOf(session: SessionWorkspaceState, turn: Collecting): Readonly<{ durationMs?: number }> {
  if (turn.userMessageId === undefined) return {};
  const start = Number(session.messages.get(turn.userMessageId)?.startedAtMs);
  if (!Number.isFinite(start)) return {};
  let end = -Infinity;
  for (const entry of turn.entries) {
    const entity = entry.type === "message" ? session.messages.get(entry.id) : session.tools.get(entry.id);
    const time = Number(entity?.endedAtMs);
    if (Number.isFinite(time) && time > end) end = time;
  }
  return Number.isFinite(end) ? { durationMs: end - start } : {};
}

/** Whether two folds of one turn show the same thing. */
function sameTurn(left: TurnView, right: TurnView): boolean {
  return left.userMessageId === right.userMessageId
    && left.running === right.running
    && left.durationMs === right.durationMs
    && sameItems(left.sections, right.sections, (a, b) => a.key === b.key &&
      (a.type !== "work" || (b.type === "work" && sameItems(a.steps, b.steps, (x, y) => x.key === y.key))))
    && sameItems(left.changes, right.changes, (a, b) => a.path === b.path && a.added === b.added && a.removed === b.removed &&
      sameItems(a.edits, b.edits, (x, y) => x === y));
}

/** Whether two lists hold equal items in the same order. */
function sameItems<T>(left: readonly T[], right: readonly T[], equal: (a: T, b: T) => boolean): boolean {
  return left.length === right.length && left.every((item, index) => equal(item, right[index]!));
}
