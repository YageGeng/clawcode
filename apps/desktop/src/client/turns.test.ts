import { expect, test } from "nsh/test";

import type { MessageId, TimestampMs, ToolCallId } from "./acp/protocol";
import type { MessageEntity, ToolCallEntity, ToolKind, TranscriptEntry } from "./domain/model";
import { fileChanges, groupTurns } from "./turns";
import { initialSessionWorkspaceState } from "./workspace/sessionState";
import type { SessionWorkspaceState } from "./workspace/sessionState";

let order = 0;

/** A message entity and its transcript entry. */
function message(id: string, role: "user" | "assistant", text: string, startMs: number, endMs = startMs): [MessageEntity, TranscriptEntry] {
  const entity = {
    messageId: id as MessageId,
    turnId: "turn" as never,
    role,
    text,
    images: [],
    reasoning: "",
    timestampMs: String(startMs) as TimestampMs,
    startedAtMs: String(startMs) as TimestampMs,
    endedAtMs: String(endMs) as TimestampMs,
    streaming: false,
  } as MessageEntity;
  return [entity, { type: "message", messageId: id as MessageId, order: { sequence: ++order, receivedOrder: order } }];
}

/** A tool entity and its transcript entry. */
function tool(id: string, kind: ToolKind, content: unknown[] = [], endMs?: number): [ToolCallEntity, TranscriptEntry] {
  const entity = {
    toolCallId: id as ToolCallId,
    title: `tool ${id}`,
    status: "completed",
    kind,
    content,
    ...(endMs === undefined ? {} : { endedAtMs: String(endMs) as TimestampMs }),
  } as ToolCallEntity;
  return [entity, { type: "tool", toolCallId: id as ToolCallId, order: { sequence: ++order, receivedOrder: order } }];
}

/** A Session projection holding `items` in order. */
function session(items: ReadonlyArray<[MessageEntity | ToolCallEntity, TranscriptEntry]>, running = false): SessionWorkspaceState {
  const messages = new Map<MessageId, MessageEntity>();
  const tools = new Map<string, ToolCallEntity>();
  for (const [entity] of items) {
    if ("messageId" in entity) messages.set(entity.messageId, entity);
    else tools.set(entity.toolCallId, entity);
  }
  return { ...initialSessionWorkspaceState, messages, tools, transcript: items.map(([, entry]) => entry), running };
}

/** A diff content item. */
function diff(path: string, oldText: string, newText: string): unknown {
  return { type: "diff", path, oldText, newText };
}

test("answer_is_the_last_assistant_message_of_an_ended_turn", () => {
  const turns = groupTurns(session([message("u1", "user", "go", 1000), message("a1", "assistant", "on it", 2000), tool("t1", "execute"), message("a2", "assistant", "done", 3000, 191000)]));
  expect(turns.length).toBe(1);
  const turn = turns[0]!;
  expect(turn.userMessageId).toBe("u1");
  expect(turn.steps).toEqual([{ type: "message", messageId: "a1" }, { type: "tools", toolCallIds: ["t1"], activities: ["ran"] }]);
  expect(turn.answerMessageId).toBe("a2");
  expect(turn.running).toBe(false);
  expect(turn.durationMs).toBe(190000);
});

test("running_turn_keeps_every_message_in_steps", () => {
  const turns = groupTurns(session([message("u1", "user", "go", 1000), message("a1", "assistant", "on it", 2000), tool("t1", "execute"), message("a2", "assistant", "still", 3000)], true));
  const turn = turns[0]!;
  expect(turn.answerMessageId).toBe(undefined);
  expect(turn.running).toBe(true);
  expect(turn.steps.length).toBe(3);
});

test("consecutive_tools_merge_into_one_group", () => {
  const merged = groupTurns(session([message("u1", "user", "go", 1), tool("t1", "execute"), tool("t2", "read"), tool("t3", "execute"), message("a1", "assistant", "done", 5)]));
  expect(merged[0]!.steps).toEqual([{ type: "tools", toolCallIds: ["t1", "t2", "t3"], activities: ["ran", "read"] }]);
  const split = groupTurns(session([message("u1", "user", "go", 1), tool("t1", "execute"), message("a1", "assistant", "hm", 2), tool("t2", "edit"), message("a2", "assistant", "done", 3)]));
  expect(split[0]!.steps.map((step) => step.type)).toEqual(["tools", "message", "tools"]);
});

test("file_changes_count_lines_per_path", () => {
  const turns = groupTurns(session([
    message("u1", "user", "go", 1),
    tool("t1", "edit", [diff("src/a.rs", "x\ny", "x\nA\nB\nC")]),
    tool("t2", "edit", [diff("src/a.rs", "keep", "keep\nD\nE")]),
    tool("t3", "edit", [diff("src/b.rs", "", "1\n2\n3\n4")]),
    message("a1", "assistant", "done", 9),
  ]));
  expect(turns[0]!.changes).toEqual([{ path: "src/a.rs", added: 5, removed: 1 }, { path: "src/b.rs", added: 4, removed: 0 }]);
  expect(fileChanges(tool("t9", "read")[0])).toEqual([]);
});

test("a_file_ending_in_a_newline_counts_its_lines", () => {
  // Most files end with a line break; it closes the last line and is not a line of its own.
  expect(fileChanges(tool("t1", "edit", [diff("new.txt", "", "1\n2\n3\n4\n")])[0])).toEqual([{ path: "new.txt", added: 4, removed: 0 }]);
  expect(fileChanges(tool("t2", "edit", [diff("gone.txt", "a\nb\n", "")])[0])).toEqual([{ path: "gone.txt", added: 0, removed: 2 }]);
});

test("file_changes_are_counted_once_per_tool_call", () => {
  // A turn is grouped again on every streamed token; a finished tool's diffs (up to megabytes) must not be re-split.
  const [entity] = tool("t1", "edit", [diff("a.txt", "x", "y")]);
  expect(fileChanges(entity)).toBe(fileChanges(entity));
});

test("turns_split_at_user_messages", () => {
  const turns = groupTurns(session([message("u1", "user", "one", 1), message("a1", "assistant", "1", 2), message("u2", "user", "two", 3), message("a2", "assistant", "2", 4)]));
  expect(turns.map((turn) => turn.key)).toEqual(["u1", "u2"]);
});
