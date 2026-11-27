import { expect, test } from "nsh/test";

import type { MessageId, TimestampMs, ToolCallId } from "../../src/client/acp/protocol";
import type { MessageEntity, ToolCallEntity, ToolKind, TranscriptEntry } from "../../src/client/domain/model";
import { fileChanges } from "../../src/client/fileChanges";
import { TurnGroups } from "../../src/client/turns";
import type { TurnView } from "../../src/client/turns";
import { initialSessionWorkspaceState, reduceSessionWorkspace, SessionWorkspaceStager } from "../../src/client/workspace/sessionState";
import type { SessionWorkspaceState } from "../../src/client/workspace/sessionState";

let order = 0;

/** A message entity and its transcript entry. */
function message(id: string, role: "user" | "assistant", text: string, startMs: number, endMs = startMs): [MessageEntity, TranscriptEntry] {
  const entity: MessageEntity = {
    messageId: id as MessageId,
    role,
    text,
    reasoning: "",
    startedAtMs: String(startMs) as TimestampMs,
    endedAtMs: String(endMs) as TimestampMs,
  };
  return [entity, { type: "message", messageId: id as MessageId, order: { sequence: ++order, receivedOrder: order } }];
}

/** A tool entity, with the changes of its `content`, and its transcript entry. */
function tool(id: string, kind: ToolKind, content: unknown[] = [], endMs?: number): [ToolCallEntity, TranscriptEntry] {
  const entity: ToolCallEntity = {
    toolCallId: id as ToolCallId,
    title: `tool ${id}`,
    status: "completed",
    kind,
    changes: fileChanges(id, content),
    ...(endMs === undefined ? {} : { endedAtMs: String(endMs) as TimestampMs }),
  };
  return [entity, { type: "tool", toolCallId: id as ToolCallId, order: { sequence: ++order, receivedOrder: order } }];
}

/** A Session projection holding `items` in order. */
function session(items: ReadonlyArray<[MessageEntity | ToolCallEntity, TranscriptEntry]>, running = false): SessionWorkspaceState {
  const stager = new SessionWorkspaceStager(initialSessionWorkspaceState);
  for (const [entity, entry] of items) {
    stager.apply("messageId" in entity ? { type: "message/upserted", message: entity, order: entry.order }
      : { type: "tool/upserted", tool: entity, order: entry.order });
  }
  stager.apply({ type: "running/changed", running });
  return stager.commit();
}

/** Folds one initial snapshot exactly as a newly opened Session does. */
function groupTurns(state: SessionWorkspaceState) {
  return new TurnGroups().update(state);
}

/** Collects steps from the work disclosures, in the order a reader can open them. */
function workSteps(turn: TurnView) {
  return turn.sections.flatMap((section) => section.type === "work" ? section.steps : []);
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
  expect(workSteps(turn)).toEqual([{ type: "message", key: "message:a1", messageId: "a1" }, { type: "tool", key: "tool:t1", toolCallId: "t1" }]);
  expect(turn.sections.at(-1)?.key).toBe("answer:a2");
  expect(turn.running).toBe(false);
  expect(turn.durationMs).toBe(190000);
});

test("running_turn_exposes_the_streaming_reply_directly", () => {
  const turns = groupTurns(session([message("u1", "user", "go", 1000), message("a1", "assistant", "on it", 2000), tool("t1", "execute"), message("a2", "assistant", "still", 3000)], true));
  const turn = turns[0]!;
  expect(turn.sections.at(-1)?.key).toBe("answer:a2");
  expect(turn.running).toBe(true);
  expect(workSteps(turn).length).toBe(2);
});

test("every_consecutive_tool_has_its_own_row_in_wire_order", () => {
  const merged = groupTurns(session([message("u1", "user", "go", 1), tool("t1", "execute"), tool("t2", "read"), tool("t3", "execute"), message("a1", "assistant", "done", 5)]));
  expect(workSteps(merged[0]!).map((step) => step.key)).toEqual(["tool:t1", "tool:t2", "tool:t3"]);
  const split = groupTurns(session([message("u1", "user", "go", 1), tool("t1", "execute"), message("a1", "assistant", "hm", 2), tool("t2", "edit"), message("a2", "assistant", "done", 3)]));
  expect(workSteps(split[0]!).map((step) => step.type)).toEqual(["tool", "message", "tool"]);
});

test("file_changes_count_lines_per_path", () => {
  const turns = groupTurns(session([
    message("u1", "user", "go", 1),
    tool("t1", "edit", [diff("src/a.rs", "x\ny", "x\nA\nB\nC")]),
    tool("t2", "edit", [diff("src/a.rs", "keep\n", "keep\nD\nE")]),
    tool("t3", "edit", [diff("src/b.rs", "", "1\n2\n3\n4")]),
    message("a1", "assistant", "done", 9),
  ]));
  expect(turns[0]!.changes.map(({ edits, ...counts }) => counts)).toEqual([{ path: "src/a.rs", added: 5, removed: 1 }, { path: "src/b.rs", added: 4, removed: 0 }]);
  expect(turns[0]!.changes[0]!.edits.map((edit) => edit.diff)).toEqual([
    { oldText: "x\ny", newText: "x\nA\nB\nC" }, { oldText: "keep\n", newText: "keep\nD\nE" },
  ]);
  expect(fileChanges("read", [{ type: "content", content: { type: "text", text: "read" } }])).toEqual([]);
});

test("a_corrected_historical_patch_refreshes_the_inspector_when_line_counts_match", () => {
  const [edit, entry] = tool("edit", "edit", [diff("src/a.rs", "old", "first")]);
  const state = session([message("u1", "user", "edit", 1), [edit, entry], message("u2", "user", "continue", 2)]);
  const groups = new TurnGroups();
  const original = groups.update(state);
  const corrected = reduceSessionWorkspace(state, { type: "tool/upserted", order: entry.order,
    tool: { ...edit, changes: fileChanges(edit.toolCallId, [diff("src/a.rs", "old", "corrected")], edit.changes) } });
  const updated = groups.update(corrected);
  expect(updated[0]!.changes[0]!.edits.map((edit) => edit.diff)).toEqual([{ oldText: "old", newText: "corrected" }]);
  expect(updated[1] === original[1]).toBe(true);
});

test("thought_only_and_tool_only_messages_keep_their_place_without_an_empty_answer", () => {
  const user = message("u1", "user", "go", 1);
  const [thought, entry] = message("a1", "assistant", "", 2);
  const [ended] = groupTurns(session([
    user, [{ ...thought, reasoning: "Read the file." }, entry],
    tool("t1", "read"), message("a2", "assistant", "", 3), tool("t2", "edit", [diff("f", "", "fixed")]),
  ]));
  expect(workSteps(ended!).map((step) => step.key)).toEqual(["thought:a1", "tool:t1", "tool:t2"]);
  expect(ended?.sections.map((section) => section.type)).toEqual(["work"]);
  expect(ended?.changes[0]?.edits[0].diff).toEqual({ oldText: "", newText: "fixed" });
});

test("snapshot_counts_match_ordered_lines_including_their_line_terminators", () => {
  const cases = [
    { old: "", next: "1\n2\n3\n4\n", added: 4, removed: 0 },
    { old: "a\nb\n", next: "", added: 0, removed: 2 },
    { old: "a\nb\n", next: "b\na\n", added: 1, removed: 1 },
    { old: "a\nb\na\n", next: "a\na\nb\n", added: 1, removed: 1 },
    { old: "a\n", next: "a", added: 1, removed: 1 },
  ];
  for (const item of cases) {
    const [edit] = fileChanges("edit", [diff("file", item.old, item.next)]);
    expect([edit!.added, edit!.removed]).toEqual([item.added, item.removed]);
  }
});

test("tool_corrections_preserve_unchanged_edits_and_reuse_identical_updates", () => {
  const a = diff("same.rs", "old", "a"), b = diff("same.rs", "old", "b"), c = diff("same.rs", "old", "c");
  const edits = fileChanges("tool", [a, b, c]);
  expect(fileChanges("tool", [a, b, c], edits) === edits).toBe(true);
  const reordered = fileChanges("tool", [c, b], edits);
  expect(reordered[0] === edits[2]).toBe(true);
  expect(reordered[1] === edits[1]).toBe(true);
  const corrected = fileChanges("tool", [b, diff("same.rs", "old", "corrected")], reordered);
  expect(corrected[0] === edits[1]).toBe(true);
  expect(corrected[1]!.id).toBe(edits[2]!.id);
});

test("a_streamed_token_redraws_no_turn", () => {
  const first = [message("u1", "user", "one", 1), message("a1", "assistant", "1", 2)] as const;
  const second = [message("u2", "user", "two", 3), message("a2", "assistant", "on", 4), tool("t1", "execute")] as const;
  const groups = new TurnGroups();
  let state = session([...first, ...second], true);
  const before = groups.update(state);
  const [streamed, entry] = second[1];
  state = reduceSessionWorkspace(state, { type: "message/upserted", message: { ...streamed, text: "on it", endedAtMs: "5" as TimestampMs }, order: entry.order });
  expect(state.turnLayout.lastUser).toEqual(second[0][1].order);
  const after = groups.update(state);
  expect(after === before).toBe(true);
  const [call, toolEntry] = tool("t2", "read");
  state = reduceSessionWorkspace(state, { type: "tool/upserted", tool: call, order: toolEntry.order });
  const grown = groups.update(state);
  expect(grown === after).toBe(false);
  expect(grown[0] === after[0]).toBe(true);
  expect(workSteps(grown[1]!).at(-1)).toEqual({ type: "tool", key: "tool:t2", toolCallId: "t2" });

  // A late change to a completed turn still updates its duration.
  const [older, oldEntry] = first[1];
  state = reduceSessionWorkspace(state, { type: "message/upserted", message: { ...older, endedAtMs: "40" as TimestampMs }, order: oldEntry.order });
  expect(groups.update(state)[0]?.durationMs).toBe(39);
  expect(groups.update(state)).toEqual(groupTurns(state));
});

test("turns_split_at_user_messages", () => {
  const turns = groupTurns(session([message("u1", "user", "one", 1), message("a1", "assistant", "1", 2), message("u2", "user", "two", 3), message("a2", "assistant", "2", 4)]));
  expect(turns.map((turn) => turn.key)).toEqual(["u1", "u2"]);
});

test("a_streamed_thought_keeps_the_grouping_index_without_visiting_any_message", () => {
  const items = Array.from({ length: 1000 }, (_, i) => [message(`u${i}`, "user", "task", i), message(`a${i}`, "assistant", "", i)]).flat();
  const [last, entry] = items[items.length - 1]!;
  items[items.length - 1] = [{ ...last, reasoning: "Think" }, entry];
  const initial = session(items, true);
  const groups = new TurnGroups();
  const before = groups.update(initial);
  const streamed = reduceSessionWorkspace(initial, { type: "message/upserted",
    message: { ...last, reasoning: "Thinking", endedAtMs: "2000" as TimestampMs }, order: entry.order });
  let lookups = 0;
  class CountedMessages extends Map<MessageId, MessageEntity> {
    /** Counts entity lookups by the display projection. */
    override get(id: MessageId) { lookups++; return super.get(id); }
  }
  expect(groups.update({ ...streamed, messages: new CountedMessages(streamed.messages) }) === before).toBe(true);
  expect(lookups).toBe(0);
});
