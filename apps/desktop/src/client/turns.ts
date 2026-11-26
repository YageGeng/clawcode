import type { MessageId, ToolCallId } from "./acp/protocol";
import type { FileChange, ToolKind } from "./domain/model";
import type { SessionWorkspaceState } from "./workspace/sessionState";

/** What a group of consecutive tool calls did, for its one-line summary. */
export type ToolActivity = "ran" | "edited" | "read" | "searched" | "fetched" | "other";

/** One item inside a turn's folded work; `key` is stable while the turn grows. */
export type WorkStep =
  | Readonly<{ type: "message"; key: string; messageId: MessageId }>
  | Readonly<{ type: "tools"; key: string; toolCallIds: readonly ToolCallId[]; activities: readonly ToolActivity[] }>;

/** A user message and everything the agent did for it. */
export type TurnView = Readonly<{
  /** The user message id, or the first entry's key for a turn without one. */
  key: string;
  userMessageId?: MessageId;
  /** Commentary and tool groups before the answer. */
  steps: readonly WorkStep[];
  /** The last assistant message once the turn ended. */
  answerMessageId?: MessageId;
  /** Changed files merged per path, in first-seen order. */
  changes: readonly FileChange[];
  running: boolean;
  /** The last entry's end minus the user message's start, once the turn ended. */
  durationMs?: number;
}>;

/** The summary activity of one tool kind. */
export function toolActivity(kind: ToolKind | undefined): ToolActivity {
  switch (kind) {
    case "execute": return "ran";
    case "edit":
    case "delete":
    case "move": return "edited";
    case "read": return "read";
    case "search": return "searched";
    case "fetch": return "fetched";
    default: return "other";
  }
}

/** The files a tool's content changed with their added and removed line counts, from its `diff` items. */
export function fileChanges(content: readonly unknown[]): readonly FileChange[] {
  return content.flatMap((item): FileChange[] => {
    if (typeof item !== "object" || item === null) return [];
    const record = item as Record<string, unknown>;
    if (record.type !== "diff" || typeof record.path !== "string" || typeof record.newText !== "string") return [];
    const oldText = typeof record.oldText === "string" ? record.oldText : "";
    return [{ path: record.path, ...lineCounts(oldText, record.newText) }];
  });
}

/**
 * Lines added and removed between two texts.
 * ponytail: multiset of lines (linear time), exact unless lines are reordered; use a real diff if counts must match git.
 */
function lineCounts(oldText: string, newText: string): Readonly<{ added: number; removed: number }> {
  // A final line break ends the last line; it does not start another one.
  const split = (text: string) => (text === "" ? [] : (text.endsWith("\n") ? text.slice(0, -1) : text).split("\n"));
  const remaining = new Map<string, number>();
  for (const line of split(oldText)) remaining.set(line, (remaining.get(line) ?? 0) + 1);
  let added = 0;
  for (const line of split(newText)) {
    const left = remaining.get(line) ?? 0;
    if (left > 0) remaining.set(line, left - 1);
    else added += 1;
  }
  let removed = 0;
  for (const left of remaining.values()) removed += left;
  return { added, removed };
}

/** A turn being collected: entries in order, plus the user message that opened it. */
type Collecting = { userMessageId?: MessageId; entries: Array<Readonly<{ type: "message"; id: MessageId } | { type: "tool"; id: ToolCallId }>> };

/**
 * Splits a Session's transcript into turns at user messages and folds each turn's work (spec §5.1).
 * Turns equal to one in `previous` are that turn, and an unchanged result is `previous` itself, so
 * a streamed token, which changes only a message's text, redraws no row.
 */
export function groupTurns(session: SessionWorkspaceState, previous: readonly TurnView[] = []): readonly TurnView[] {
  const collected: Collecting[] = [];
  for (const entry of session.transcript) {
    if (entry.type === "message") {
      const message = session.messages.get(entry.messageId);
      if (message === undefined) continue;
      if (message.role === "user") {
        collected.push({ userMessageId: message.messageId, entries: [] });
        continue;
      }
      if (collected.length === 0) collected.push({ entries: [] });
      collected.at(-1)!.entries.push({ type: "message", id: message.messageId });
    } else {
      if (collected.length === 0) collected.push({ entries: [] });
      collected.at(-1)!.entries.push({ type: "tool", id: entry.toolCallId });
    }
  }
  const previousByKey = new Map(previous.map((turn) => [turn.key, turn]));
  const turns = collected.map((turn, index) => {
    const next = foldTurn(session, turn, index, index === collected.length - 1);
    const kept = previousByKey.get(next.key);
    return kept !== undefined && sameTurn(kept, next) ? kept : next;
  });
  return turns.length === previous.length && turns.every((turn, index) => turn === previous[index]) ? previous : turns;
}

/** One collected turn as the transcript shows it; only the newest turn can still be running. */
function foldTurn(session: SessionWorkspaceState, turn: Collecting, index: number, newest: boolean): TurnView {
  const running = session.running && newest;
  let lastMessage: MessageId | undefined;
  for (const item of turn.entries) if (item.type === "message") lastMessage = item.id;
  const answerMessageId = running ? undefined : lastMessage;
  const steps: WorkStep[] = [];
  // The open tool group, built in place: copying it per call would be quadratic in its length.
  let group: { key: string; toolCallIds: ToolCallId[]; activities: ToolActivity[] } | undefined;
  const changes = new Map<string, { added: number; removed: number }>();
  for (const item of turn.entries) {
    if (item.type === "message") {
      group = undefined;
      if (item.id !== answerMessageId) steps.push({ type: "message", key: item.id, messageId: item.id });
      continue;
    }
    const tool = session.tools.get(item.id);
    const activity = toolActivity(tool?.kind);
    if (group === undefined) {
      group = { key: `tools:${item.id}`, toolCallIds: [], activities: [] };
      steps.push({ type: "tools", ...group });
    }
    group.toolCallIds.push(item.id);
    if (!group.activities.includes(activity)) group.activities.push(activity);
    for (const change of tool?.changes ?? []) {
      const merged = changes.get(change.path) ?? { added: 0, removed: 0 };
      changes.set(change.path, { added: merged.added + change.added, removed: merged.removed + change.removed });
    }
  }
  const first = turn.entries[0];
  const firstKey = first === undefined ? `turn-${index}` : `${first.type}:${first.id}`;
  return {
    key: turn.userMessageId ?? firstKey,
    ...(turn.userMessageId === undefined ? {} : { userMessageId: turn.userMessageId }),
    steps,
    ...(answerMessageId === undefined ? {} : { answerMessageId }),
    changes: [...changes].map(([path, counts]) => ({ path, ...counts })),
    running,
    ...(running ? {} : durationOf(session, turn)),
  };
}

/** How long an ended turn took: its last entry's end minus its user message's start. */
function durationOf(session: SessionWorkspaceState, turn: Collecting): Readonly<{ durationMs?: number }> {
  const start = turn.userMessageId === undefined ? undefined : Number(session.messages.get(turn.userMessageId)?.startedAtMs);
  const ends = turn.entries.map((item) =>
    Number(item.type === "message" ? session.messages.get(item.id)?.endedAtMs : session.tools.get(item.id)?.endedAtMs),
  ).filter((value) => Number.isFinite(value));
  if (start === undefined || !Number.isFinite(start) || ends.length === 0) return {};
  return { durationMs: Math.max(...ends) - start };
}

/** Whether two folds of one turn show the same thing. */
function sameTurn(left: TurnView, right: TurnView): boolean {
  return left.userMessageId === right.userMessageId
    && left.answerMessageId === right.answerMessageId
    && left.running === right.running
    && left.durationMs === right.durationMs
    && sameItems(left.steps, right.steps, (a, b) => a.key === b.key && (a.type === "message" || (b.type === "tools"
      && sameItems(a.toolCallIds, b.toolCallIds, (x, y) => x === y) && sameItems(a.activities, b.activities, (x, y) => x === y))))
    && sameItems(left.changes, right.changes, (a, b) => a.path === b.path && a.added === b.added && a.removed === b.removed);
}

/** Whether two lists hold equal items in the same order. */
function sameItems<T>(left: readonly T[], right: readonly T[], equal: (a: T, b: T) => boolean): boolean {
  return left.length === right.length && left.every((item, index) => equal(item, right[index]!));
}
