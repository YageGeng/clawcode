import { expect, test } from "nsh/test";

import type { MessageId, SessionId, SessionUpdateNotification, ToolCallId } from "../../src/client/acp/protocol";
import { TurnGroups } from "../../src/client/turns";
import { initialSessionWorkspaceState, reduceSessionWorkspace, SessionWorkspaceStager } from "../../src/client/workspace/sessionState";
import { initialWorkspaceState, reduceWorkspace, sessionWorkspace, withSessionWorkspaces } from "../../src/client/workspace/state";
import type { WorkspaceAction, WorkspaceState } from "../../src/client/workspace/state";
import { SessionUpdateRouter } from "../../src/client/workspace/updateRouter";
import type { WorkspaceStoreAccess } from "../../src/client/workspace/updateRouter";

test("router_dispatches_through_the_injected_store", () => {
  let state: WorkspaceState = initialWorkspaceState;
  const actions: WorkspaceAction[] = [];
  const store: WorkspaceStoreAccess = {
    getState: () => state,
    dispatch: (action) => {
      actions.push(action);
      state = reduceWorkspace(state, action);
    },
  };
  const idle = () => Promise.resolve();
  const router = new SessionUpdateRouter("clawcode", store, { branch: idle, runtime: idle, sessions: idle });
  const sessionId = "session-1" as SessionId;
  const notification = {
    sessionId,
    update: {
      sessionUpdate: "agent_message_chunk",
      messageId: "message-1",
      content: { type: "text", text: "你好 🌍\nline two" },
      _meta: { clawcode: { turnId: "turn-1", timestampMs: "1700000000000", sequence: 1 } },
    },
  } as unknown as SessionUpdateNotification;

  router.applyMany([notification]);

  expect(actions.length).toBeGreaterThan(0);
  const session = state.sessionWorkspaces.get(sessionId);
  expect(session?.transcript.length).toBe(1);
  expect(JSON.stringify([...(session?.messages.values() ?? [])])).toContain("你好 🌍\\nline two");
});

test("settled_questions_share_a_replay_ledger_and_noops_retain_the_snapshot", () => {
  const replay = new SessionWorkspaceStager(initialSessionWorkspaceState);
  for (let i = 0; i < 8000; i++) replay.apply({ type: "interactions/changed", change: { type: "settled", ids: [String(i)] } });
  const published = replay.commit();
  expect(published.interactions.settled.size).toBe(8000);
  expect(initialSessionWorkspaceState.interactions.settled.size).toBe(0);
  const duplicate = reduceSessionWorkspace(published, { type: "interactions/changed", change: { type: "settled", ids: ["42"] } });
  expect(duplicate === published).toBe(true);
  const next = reduceSessionWorkspace(published, { type: "interactions/changed", change: { type: "settled", ids: ["next"] } });
  expect(next.interactions.settled.size).toBe(8001);
  expect(published.interactions.settled.has("next")).toBe(false);
});

test("sidebar_attention_changes_only_with_open_question_membership", () => {
  const id = "s1" as SessionId;
  const question = { key: { requestId: "q", index: 0 }, title: "One?", options: [], multiSelect: false };
  const asked = reduceSessionWorkspace(initialSessionWorkspaceState,
    { type: "interactions/changed", change: { type: "asked", questions: [question] } });
  const first = withSessionWorkspaces(initialWorkspaceState, [[id, asked]]);
  expect(first.awaitingAnswers.has(id)).toBe(true);
  const running = reduceWorkspace(first, { type: "session/updated", sessionId: id, action: { type: "running/changed", running: true } });
  expect(running.awaitingAnswers === first.awaitingAnswers).toBe(true);
  const queued = reduceWorkspace(running, { type: "session/updated", sessionId: id,
    action: { type: "interactions/changed", change: { type: "queued", ids: ["q:0"] } } });
  expect(queued.awaitingAnswers.has(id)).toBe(false);
  expect(first.awaitingAnswers.has(id)).toBe(true);
});

/** Routes live and replay frames through the production reducer for the listed Sessions. */
function routedWorkspace(...sessionIds: SessionId[]) {
  let state = reduceWorkspace(initialWorkspaceState, {
    type: "session/listed", sessions: sessionIds.map((sessionId) => ({ sessionId, cwd: "/", title: sessionId })),
  });
  const idle = () => Promise.resolve();
  const router = new SessionUpdateRouter("clawcode", {
    getState: () => state,
    dispatch: (action) => { state = reduceWorkspace(state, action); },
  }, { branch: idle, runtime: idle, sessions: idle });
  let sequence = 0;
  return {
    router,
    session: (sessionId: SessionId) => sessionWorkspace(state, sessionId),
    notices: () => state.notices,
    /** Delivers one frame with the ACP event metadata required by message projections. */
    send(sessionId: SessionId, ...updates: Record<string, unknown>[]) {
      router.applyMany(updates.map((update) => ({
        sessionId, update, _meta: { clawcode: { turnId: "turn", timestampMs: "1", sequence: ++sequence } },
      })));
    },
  };
}

test("full_replay_reuses_surviving_edits_and_drops_unreported_content", async () => {
  const sessionId = "session" as SessionId;
  const workspace = routedWorkspace(sessionId);
  const call = { sessionUpdate: "tool_call_update", toolCallId: "call" as ToolCallId };
  const a = { type: "diff", path: "file", oldText: "", newText: "A" };
  const b = { ...a, newText: "B" };
  const edits = () => workspace.session(sessionId).tools.get(call.toolCallId)!.changes;
  workspace.send(sessionId, { ...call, content: [a, b] });
  const selected = edits()[1];
  await workspace.router.replay(sessionId, async () => {
    workspace.send(sessionId, call, { ...call, content: [b] });
  });
  expect(edits().length).toBe(1);
  expect(edits()[0] === selected).toBe(true);
  await workspace.router.replay(sessionId, async () => { workspace.send(sessionId, call); });
  expect(edits()).toEqual([]);
});

test("tool_input_output_and_content_patch_independently", () => {
  const id = "session" as SessionId;
  const workspace = routedWorkspace(id);
  const toolCallId = "call" as ToolCallId;
  const call = { sessionUpdate: "tool_call_update", toolCallId };
  const tool = () => workspace.session(id).tools.get(toolCallId)!;
  const content = [{ type: "content", content: { type: "image", mimeType: "image/png", data: "AQID" } }];
  const rawOutput = { content: { blocks: [{ type: "image", mime_type: "image/png", data: "AQID" }] }, extra: 3 };
  workspace.send(id, { ...call, name: "mcp_image", rawInput: { query: "tree" } },
    { ...call, content, rawOutput }, { ...call, status: "completed" });
  expect(tool()).toMatchObject({ name: "mcp_image", rawInput: { query: "tree" }, rawOutput, content });
  workspace.send(id, { ...call, content: [] });
  expect(tool().content).toEqual([]);
  expect(tool().rawOutput).toEqual(rawOutput);
  workspace.send(id, { ...call, name: null, rawOutput: false, rawInput: null });
  expect(tool().name).toBeUndefined();
  expect(tool().rawInput).toBeUndefined();
  expect(tool().rawOutput).toBe(false);
  workspace.send(id, { ...call, rawOutput: null });
  expect(tool().rawOutput).toBeUndefined();
});

test("assistant_media_keeps_its_position_across_streaming_authoritative_replacement_and_clear", () => {
  const id = "session" as SessionId;
  const workspace = routedWorkspace(id);
  const messageId = "message" as MessageId;
  const image = { type: "image", mimeType: "image/png", data: "AQID" };
  const resource = { type: "resource_link", uri: "file:///answer", name: "Answer" };
  workspace.send(id, { sessionUpdate: "agent_thought_chunk", messageId, content: { type: "text", text: "thinking" } });
  const layout = workspace.session(id).turnLayout;
  workspace.send(id, { sessionUpdate: "agent_message_chunk", messageId, content: image },
    { sessionUpdate: "agent_message_chunk", messageId, content: { type: "text", text: "🌳" } },
    { sessionUpdate: "agent_message_chunk", messageId, content: resource });
  const message = () => workspace.session(id).messages.get(messageId)!;
  const media = message().attachments;
  expect(media).toEqual([{ textOffset: 0, block: image }, { textOffset: 2, block: resource }]);
  expect(workspace.session(id).turnLayout === layout).toBe(false);
  workspace.send(id, { sessionUpdate: "agent_message_chunk", messageId, content: { type: "text", text: " done" } });
  expect(message().attachments === media).toBe(true);
  expect(message().text).toBe("🌳 done");
  workspace.send(id, { sessionUpdate: "agent_message", messageId, content: [{ type: "text", text: "A" }, resource,
    { type: "text", text: "B" }, image, { type: "text", text: "C" }] },
    { sessionUpdate: "agent_message", messageId });
  expect(message()).toMatchObject({ text: "ABC", reasoning: "thinking",
    attachments: [{ textOffset: 1, block: resource }, { textOffset: 2, block: image }] });
  workspace.send(id, { sessionUpdate: "agent_message", messageId, content: null });
  expect(message().attachments?.length ?? 0).toBe(0);
  expect(message().text).toBe("");
});

test("user_shell_keeps_the_previous_final_reply_and_its_own_tool_during_live_and_replay", async () => {
  const id = "shell" as SessionId;
  const workspace = routedWorkspace(id);
  const history = [
    { sessionUpdate: "user_message", messageId: "u", content: [{ type: "text", text: "Build" }] },
    { sessionUpdate: "agent_message", messageId: "a", content: [{ type: "text", text: "Built." }] },
    { sessionUpdate: "user_message", messageId: "sh", content: [{ type: "text", text: "!!pwd" }],
      _meta: { clawcode: { origin: "user_bash", turnId: "shell", timestampMs: "3", sequence: 3 } } },
    { sessionUpdate: "tool_call_update", toolCallId: "sh", name: "bash", status: "completed" },
  ];
  const groups = new TurnGroups();
  workspace.send(id, ...history.slice(0, 2));
  const first = groups.update(workspace.session(id))[0];
  workspace.send(id, ...history.slice(2));
  const live = groups.update(workspace.session(id));
  expect(live[0] === first).toBe(true);
  expect(live.map((turn) => [turn.key, turn.sections.map((section) => section.key)])).toEqual([
    ["u", ["answer:a"]], ["sh", ["work:sh"]],
  ]);
  expect(workspace.session(id).messages.get("sh" as MessageId)?.text).toBe("!!pwd");
  expect(workspace.session(id).tools.get("sh" as ToolCallId)?.name).toBe("bash");
  // A resumed host sends fresh replay sequence numbers for all messages, including the shell metadata.
  await workspace.router.replay(id, async () => { workspace.send(id, ...history.map((update, i) => ({
    ...update, _meta: { clawcode: { ...update._meta?.clawcode, turnId: "replay", timestampMs: "1", sequence: i + 1 } },
  }))); });
  expect(groups.update(workspace.session(id)).map((turn) => turn.sections)).toEqual(live.map((turn) => turn.sections));
});

test("durable_question_decisions_keep_run_boundaries_and_mid_run_positions_including_trailing_skips", () => {
  const id = "questions" as SessionId;
  const workspace = routedWorkspace(id);
  /** Projects an answer or skip as the kernel's persisted extension message. */
  const decision = (messageId: string, index: number, answer?: string, extensionId = "clawcode") => ({
    sessionUpdate: "_clawcode/event", payload: { event: "message_end", message: {
      message_id: messageId, content: { type: "extension", extension: { extension_id: extensionId, custom_type: "question_reply",
        details: { items: [{ request_id: "ask", index, question: `Option ${index}?`, answer }] } } },
    } },
  });
  const first = decision("steer", 0, "yes");
  workspace.send(id, { sessionUpdate: "user_message", messageId: "prompt", content: [{ type: "text", text: "Build" }] },
    { sessionUpdate: "tool_call_update", toolCallId: "ask" }, first,
    { sessionUpdate: "tool_call_update", toolCallId: "build" },
    { sessionUpdate: "agent_message", messageId: "done", content: [{ type: "text", text: "Built." }] }, decision("skip", 1),
    { sessionUpdate: "_clawcode/event", payload: { event: "run_end", run_id: "original" } },
    first, decision("other", 2, "foreign", "other"), decision("new", 2, "continue"),
    { sessionUpdate: "agent_message", messageId: "new-answer", content: [{ type: "text", text: "Continuing." }] });
  const session = workspace.session(id);
  expect(session.messages.has("other" as MessageId)).toBe(false);
  expect(session.messages.get("skip" as MessageId)?.decisions).toEqual([{ id: "ask:1", question: "Option 1?" }]);
  expect(session.messages.get("new" as MessageId)?.decisions).toEqual([{ id: "ask:2", question: "Option 2?", answer: "continue" }]);
  const turns = new TurnGroups().update(session);
  expect(turns.map((turn) => [turn.userMessageId, turn.sections.map((section) => section.key)])).toEqual([
    ["prompt", ["work:prompt", "decision:steer", "work:steer", "answer:done", "decision:skip"]],
    ["new", ["answer:new-answer"]],
  ]);
});

test("terminal_bytes_are_scoped_to_each_session_and_reset_on_branch_replay", async () => {
  const a = "a" as SessionId;
  const b = "b" as SessionId;
  const workspace = routedWorkspace(a, b);
  const patch = { sessionUpdate: "terminal_update", terminalId: "shared" };
  const chunk = { sessionUpdate: "terminal_output_chunk", terminalId: "shared" };
  const terminal = (id: SessionId) => workspace.session(id).terminals.get("shared");
  // These fragments split the UTF-8 encodings of 🌍 and é between RPC frames.
  workspace.send(a, { ...patch, command: "print", cwd: "/a" }, { ...chunk, data: "8J8=" });
  workspace.send(b, { ...chunk, data: "ww==" });
  workspace.send(a, { ...chunk, data: "jI0=" }, { ...patch, exitStatus: { exitCode: 0 } });
  workspace.send(b, { ...chunk, data: "qQ==" });
  expect(terminal(a)).toMatchObject({ command: "print", cwd: "/a", output: "🌍", exitStatus: { exitCode: 0 } });
  expect(terminal(b)?.output).toBe("é");
  workspace.send(a, { ...patch, output: null, cwd: null, exitStatus: null }, { ...chunk, data: "8J8=" });
  expect(terminal(a)).toMatchObject({ command: "print", output: "" });
  expect(terminal(a)?.cwd).toBeUndefined();
  expect(terminal(a)?.exitStatus).toBeUndefined();
  workspace.send(a, { sessionUpdate: "_clawcode/event", payload: { event: "branch_navigated" } },
    { ...chunk, data: "jI0=" });
  expect(terminal(a)).toBeUndefined();
  await workspace.router.replay(a, async () => {
    workspace.send(a, { ...chunk, data: "WA==" });
    expect(terminal(a)).toBeUndefined();
  });
  expect(terminal(a)?.output).toBe("X");
  expect(terminal(a)?.command).toBeUndefined();
  workspace.send(a, { ...chunk, data: "8J8=" }, { ...patch, output: { data: "b2s=" } }, { ...chunk, data: "IQ==" });
  expect(terminal(a)?.output).toBe("ok!");
  expect(terminal(b)?.output).toBe("é");
});

test("discarding_a_frame_does_not_consume_partial_terminal_bytes", () => {
  const id = "session" as SessionId;
  const workspace = routedWorkspace(id);
  const chunk = { sessionUpdate: "terminal_output_chunk", terminalId: "terminal" };
  workspace.send(id, { ...chunk, data: "8J8=" });
  // The first notification completes 🌍 in the draft. Corrupt recovery metadata then aborts the entire frame.
  expect(() => workspace.router.applyMany([
    { sessionId: id, update: { ...chunk, data: "jI0=" } },
    { sessionId: id, update: { ...chunk, data: "WA==" }, _meta: { clawcode: { sessionRecovery: {} } } },
  ])).toThrow("projection group");
  expect(workspace.session(id).terminals.get("terminal")?.output).toBe("");
  workspace.send(id, { ...chunk, data: "jI0=" });
  expect(workspace.session(id).terminals.get("terminal")?.output).toBe("🌍");
});

/** Builds the atomic projection envelope used by the production backend for an active Run. */
function recoverableGroup(sessionId: SessionId, sequence: number, updates: readonly Record<string, unknown>[], operationPhase?: "start" | "end"): SessionUpdateNotification[] {
  return updates.map((update, projectionIndex) => ({
    sessionId, update, _meta: { clawcode: {
      turnId: "turn", timestampMs: "1", sequence,
      sessionRecovery: { runId: "run", projectionIndex, projectionCount: updates.length, operationPhase },
    } },
  }));
}

const signInFailure = {
  sessionUpdate: "state_update", state: "idle", stopReason: "error",
  error: { code: -32000, message: "provider answered 401", data: { methodId: "chatgpt" } },
};

/** Keeps a live authentication failure visible while post-resume question reservations are pending. */
test("a_live_run_error_after_wire_replay_completion_precedes_question_publication", async () => {
  const sessionId = "session" as SessionId;
  const workspace = routedWorkspace(sessionId);
  let finishReservations!: () => void;
  const reservations = new Promise<void>((resolve) => { finishReservations = resolve; });
  const replay = workspace.router.replay(sessionId, async (history) => {
    // The server replays the active journal, ends wire history, then reports current state before resume returns.
    workspace.router.applyMany([
      ...recoverableGroup(sessionId, 1, [{ sessionUpdate: "_clawcode/event", payload: { event: "run_start", run_id: "run" } }], "start"),
      ...recoverableGroup(sessionId, 2, [{ sessionUpdate: "tool_call_update", toolCallId: "ask",
        rawOutput: { content: { details: { type: "user_questions", request_id: "ask", questions: [{ title: "Continue?" }] } } } }]),
      { sessionId, update: { sessionUpdate: "replay_complete" } },
      { sessionId, update: { sessionUpdate: "state_update", state: "running" } },
    ]);
    expect(history.hasQuestions()).toBe(true);
    // The host keeps the history private until the pending_messages response reserves its questions.
    await reservations;
    history.apply({ type: "interactions/changed", change: { type: "queued", ids: ["ask:0"] } });
  });
  try {
    expect(workspace.session(sessionId).running).toBe(true);
    expect(workspace.session(sessionId).interactions.questions).toEqual([]);
    const settled = recoverableGroup(sessionId, 3, [
      { sessionUpdate: "_clawcode/event", payload: { event: "agent_settled" } }, signInFailure,
    ]);
    workspace.router.applyMany(settled.slice(0, 1));
    expect(workspace.session(sessionId).running).toBe(true);
    expect(workspace.notices()).toEqual([]);
    workspace.router.applyMany(settled.slice(1));
    expect(workspace.session(sessionId).running).toBe(false);
    expect(workspace.notices().map((notice) => [notice.signIn, notice.description])).toEqual([["chatgpt", "provider answered 401"]]);
    expect(workspace.session(sessionId).interactions.questions).toEqual([]);
  } finally {
    finishReservations();
    await replay;
  }
  expect(workspace.notices().length).toBe(1);
  expect(workspace.session(sessionId).interactions.questions.map(({ title }) => title)).toEqual(["Continue?"]);
  expect(workspace.session(sessionId).interactions.queued.has("ask:0")).toBe(true);
});

/** Separates historical notices from live ones in wire order and resets the boundary for every Session replay. */
test("wire_replay_completion_is_ordered_session_local_and_reset_for_legacy_replays", async () => {
  const sessionId = "session" as SessionId;
  const other = "other" as SessionId;
  const workspace = routedWorkspace(sessionId, other);
  await workspace.router.replay(sessionId, async () => {
    // A different Session's marker cannot end this Session's historical suppression.
    workspace.send(other, { sessionUpdate: "replay_complete" });
    workspace.send(sessionId, signInFailure);
    expect(workspace.notices()).toEqual([]);
    workspace.send(sessionId, signInFailure, { sessionUpdate: "replay_complete" }, signInFailure);
    expect(workspace.notices().length).toBe(1);
  });
  await workspace.router.replay(sessionId, async () => {
    // An older peer without replay_complete keeps the callback-based suppression, even after a modern replay.
    workspace.send(sessionId, signInFailure);
    expect(workspace.notices().length).toBe(1);
  });
  workspace.send(sessionId, signInFailure);
  expect(workspace.notices().length).toBe(2);
});

/** A superseded branch stays quiet whether navigation happens before or after its replay marker. */
test("superseded_branches_cannot_raise_notices_after_wire_replay_completion", async () => {
  const sessionId = "session" as SessionId;
  for (const navigateFirst of [true, false]) {
    const workspace = routedWorkspace(sessionId);
    const navigate = { sessionUpdate: "_clawcode/event", payload: { event: "branch_navigated" } };
    const complete = { sessionUpdate: "replay_complete" };
    await workspace.router.replay(sessionId, async () => {
      workspace.send(sessionId, ...(navigateFirst ? [navigate, complete] : [complete, navigate]), signInFailure);
      expect(workspace.notices()).toEqual([]);
    });
    workspace.send(sessionId, signInFailure);
    expect(workspace.notices()).toEqual([]);
    expect(workspace.router.needsBranchReplay(sessionId)).toBe(true);
    await workspace.router.replay(sessionId, async () => { workspace.send(sessionId, complete, signInFailure); });
    expect(workspace.notices().length).toBe(1);
  }
});

test("a_live_run_error_raises_a_notice_and_a_replayed_one_does_not", async () => {
  const sessionId = "session" as SessionId;
  const workspace = routedWorkspace(sessionId);
  const failed = {
    sessionUpdate: "state_update", state: "idle", stopReason: "error",
    error: { code: -32000, message: "provider answered 401", data: { methodId: "chatgpt" } },
  };
  workspace.send(sessionId, failed);
  expect(workspace.notices().map((notice) => [notice.signIn, notice.description])).toEqual([["chatgpt", "provider answered 401"]]);
  // Any other error says what went wrong without offering a sign-in.
  workspace.send(sessionId, { ...failed, error: { code: -32603, message: "boom" } });
  expect(workspace.notices()[1]?.signIn).toBe(undefined);
  await workspace.router.replay(sessionId, async () => { workspace.send(sessionId, failed); });
  expect(workspace.notices().length).toBe(2);
});
