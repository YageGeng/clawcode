import type { MessageId, ToolCallId } from "./acp/protocol";
import type { ToolCallEntity, ToolKind } from "./domain/model";
import type { SessionWorkspaceState } from "./workspace/sessionState";

/** What a group of consecutive tool calls did, for its one-line summary. */
export type ToolActivity = "ran" | "edited" | "read" | "searched" | "fetched" | "other";

/** One item inside a turn's folded work. */
export type WorkStep =
  | Readonly<{ type: "message"; messageId: MessageId }>
  | Readonly<{ type: "tools"; toolCallIds: readonly ToolCallId[]; activities: readonly ToolActivity[] }>;

/** One file a tool changed, from its diff content. */
export type FileChange = Readonly<{ path: string; added: number; removed: number }>;

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

/** Counted changes by tool call entity: entities are replaced, never mutated, so a cached count stays right. */
const countedChanges = new WeakMap<ToolCallEntity, readonly FileChange[]>();

/** The files `tool` changed with their added and removed line counts, from its `diff` content items. */
export function fileChanges(tool: ToolCallEntity): readonly FileChange[] {
  const cached = countedChanges.get(tool);
  if (cached !== undefined) return cached;
  const changes = tool.content.flatMap((item): FileChange[] => {
    if (typeof item !== "object" || item === null) return [];
    const record = item as Record<string, unknown>;
    if (record.type !== "diff" || typeof record.path !== "string" || typeof record.newText !== "string") return [];
    const oldText = typeof record.oldText === "string" ? record.oldText : "";
    return [{ path: record.path, ...lineCounts(oldText, record.newText) }];
  });
  countedChanges.set(tool, changes);
  return changes;
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

/** Splits a Session's transcript into turns at user messages and folds each turn's work (spec §5.1). */
export function groupTurns(session: SessionWorkspaceState): readonly TurnView[] {
  const collected: Collecting[] = [];
  for (const entry of session.transcript) {
    if (entry.type === "message") {
      const message = session.messages.get(entry.messageId);
      if (message === undefined) continue;
      if (message.role === "user") {
        collected.push({ userMessageId: message.messageId, entries: [] });
        continue;
      }
      if (message.role !== "assistant") continue;
      if (collected.length === 0) collected.push({ entries: [] });
      collected.at(-1)!.entries.push({ type: "message", id: message.messageId });
    } else if (entry.type === "tool") {
      if (collected.length === 0) collected.push({ entries: [] });
      collected.at(-1)!.entries.push({ type: "tool", id: entry.toolCallId });
    }
  }
  return collected.map((turn, index) => {
    // Only the newest turn can still be running.
    const running = session.running && index === collected.length - 1;
    const lastMessage = [...turn.entries].reverse().find((item) => item.type === "message");
    const answerMessageId = !running && lastMessage !== undefined ? (lastMessage.id as MessageId) : undefined;
    const steps: WorkStep[] = [];
    const changes = new Map<string, { added: number; removed: number }>();
    for (const item of turn.entries) {
      if (item.type === "message") {
        if (item.id !== answerMessageId) steps.push({ type: "message", messageId: item.id });
        continue;
      }
      const tool = session.tools.get(item.id);
      const activity = toolActivity(tool?.kind);
      const last = steps.at(-1);
      if (last?.type === "tools") {
        steps[steps.length - 1] = {
          type: "tools",
          toolCallIds: [...last.toolCallIds, item.id],
          activities: last.activities.includes(activity) ? last.activities : [...last.activities, activity],
        };
      } else {
        steps.push({ type: "tools", toolCallIds: [item.id], activities: [activity] });
      }
      for (const change of tool === undefined ? [] : fileChanges(tool)) {
        const merged = changes.get(change.path) ?? { added: 0, removed: 0 };
        changes.set(change.path, { added: merged.added + change.added, removed: merged.removed + change.removed });
      }
    }
    const firstKey = turn.entries[0] === undefined ? `turn-${index}` : `${turn.entries[0].type}:${turn.entries[0].id}`;
    return {
      key: turn.userMessageId ?? firstKey,
      ...(turn.userMessageId === undefined ? {} : { userMessageId: turn.userMessageId }),
      steps,
      ...(answerMessageId === undefined ? {} : { answerMessageId }),
      changes: [...changes].map(([path, counts]) => ({ path, ...counts })),
      running,
      ...(running ? {} : durationOf(session, turn)),
    };
  });
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
