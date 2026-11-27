import { expect, test } from "nsh/test";

import { ScriptedAgent } from "../support/scriptedAgent";
import type { SessionId } from "../../src/client/acp/protocol";
import { createAppStore } from "../../src/stores/app";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";

const session = "s1" as SessionId;
const questions = { type: "user_questions", request_id: "call-7", questions: [
  { title: "Database?", options: ["SQLite", "Postgres"] }, { title: "Notes?" }, { title: "Where?", multi_select: true, options: ["Dev", "CI"] },
] };

/** The durable extension message the kernel emits for answers and skips. */
function reply(index: number, extensionId = "clawcode") {
  return { content: { type: "extension", extension: { extension_id: extensionId, custom_type: "question_reply",
    details: { items: [{ request_id: "call-7", index, question: "Database?", answer: "SQLite" }] } } } };
}

test("questions_open_only_after_persistence_and_track_other_clients_queued_and_recorded_answers", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  const host = createHostStore(LOCAL_HOST, agent.transport);
  await host.start();
  await host.openSession(session);
  const tool = { sessionUpdate: "tool_call_update", toolCallId: "tool", status: "completed" };
  agent.push(agent.update(session, { ...tool, rawOutput: { details: questions } }));
  expect(host.openQuestions(session)).toEqual([]);
  agent.push(agent.update(session, { ...tool, rawOutput: { content: { details: questions } } }));
  expect(host.openQuestions(session).map((q) => q.title)).toEqual(["Database?", "Notes?", "Where?"]);
  /** Publishes a product envelope without embedding question state in a tool delta. */
  const event = (payload: unknown) => agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload }));
  event({ event: "queue_changed", pending: { steering: [{ message: reply(0) }], followUp: [] } });
  expect(host.openQuestions(session).map((q) => q.key.index)).toEqual([1, 2]);
  event({ event: "queue_changed", pending: { steering: [], followUp: [] } });
  event({ event: "message_end", message: reply(0, "other-extension") });
  expect(host.openQuestions(session).length).toBe(3);
  event({ event: "message_end", message: reply(1) });
  expect(host.openQuestions(session).map((q) => q.key.index)).toEqual([0, 2]);
});

test("question_answers_steer_an_active_turn_and_skips_send_one_batch", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  const host = createHostStore({ id: "remote", kind: "remote", name: "Remote" }, agent.transport);
  agent.replays.set(session, [agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: "tool",
    rawOutput: { content: { details: questions } } })]);
  await host.start();
  await host.openSession(session);
  agent.push(agent.state(session, "running"));
  const keys = host.openQuestions(session).map((item) => item.key);
  agent.onPrompt = () => {};
  const pending = host.answerQuestion(session, keys[0]!, "SQLite");
  const sent = agent.calls("session/prompt")[0]!;
  expect(sent.params.prompt).toEqual([{ type: "text", text: "SQLite" }]);
  const meta = (sent.params._meta as { clawcode: Record<string, unknown> }).clawcode;
  expect(meta.questionReply).toEqual({ requestId: "call-7", index: 0 });
  expect(meta.whenBusy).toBe(undefined);
  await expect(host.answerQuestion(session, keys[0]!, "again")).rejects.toThrow();
  agent.fail(sent.id!, "try again");
  await expect(pending).rejects.toThrow();
  expect(host.openQuestions(session).length).toBe(3);
  agent.onPrompt = (call) => agent.respond(call.id, { userMessageId: "answer" });
  await host.answerQuestion(session, keys[0]!, "SQLite");
  await host.dismissQuestions(session, keys.slice(1));
  expect(agent.calls("_clawcode/session/question/dismiss")[0]!.params).toEqual({ sessionId: session, items: keys.slice(1) });
  expect(host.openQuestions(session)).toEqual([]);
  expect(host.running(session)).toBe(true);
});

test("dismiss_settles_only_acknowledged_skips_even_when_queue_notifications_arrive_late", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  agent.replays.set(session, [agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: "tool",
    rawOutput: { content: { details: questions } } })]);
  const host = createHostStore(LOCAL_HOST, agent.transport);
  await host.start();
  await host.openSession(session);
  const keys = host.openQuestions(session).map(({ key }) => key);
  const send = agent.transport.send.bind(agent.transport);
  let acknowledgement: unknown = { skipped: [keys[1]] };
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (frame.method === "_clawcode/session/question/dismiss") agent.respond(frame.id, acknowledgement);
    else send(line);
  };
  await host.dismissQuestions(session, keys);
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "queue_changed",
    pending: { steering: [{ message: reply(0) }], followUp: [] } } }));
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([2]);
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "queue_changed",
    pending: { steering: [], followUp: [] } } }));
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([0, 2]);

  // A held skip is reversible until its message is durable, including on the requesting client.
  acknowledgement = { skipped: [keys[0]], reservedQuestions: [keys[0]] };
  await host.dismissQuestions(session, [keys[0]!]);
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([2]);
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "queue_changed",
    pending: { reservedQuestions: [] } } }));
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([0, 2]);

  // Older peers acknowledge the RPC without identifying skips. Only a persisted message ends the question.
  acknowledgement = {};
  await host.dismissQuestions(session, [keys[0]!]);
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([0, 2]);
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "message_end", message: reply(0) } }));
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([2]);
});

test("branch_navigation_discards_old_history_and_drains_an_inflight_resume_before_replaying", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  /** Builds branch history with distinct question and plan content. */
  const history = (branch: string) => [
    agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: branch,
      rawOutput: { content: { details: { ...questions, request_id: branch } } } }),
    agent.update(session, { sessionUpdate: "plan_update", plan: { planId: "plan-mode", type: "markdown", content: branch } }),
  ];
  agent.replays.set(session, history("old"));
  const host = createHostStore(LOCAL_HOST, agent.transport);
  await host.start();
  await host.openSession(session);
  const send = agent.transport.send.bind(agent.transport);
  let resumeId: number | undefined;
  const started = new Promise<void>((resolve) => {
    agent.transport.send = (line) => {
      const frame = JSON.parse(line);
      if (frame.method === "session/resume" && resumeId === undefined) {
        resumeId = frame.id;
        resolve();
      } else send(line);
    };
  });
  /** Reports another client's navigation while the active branch may still be replaying. */
  const navigate = () => agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "branch_navigated" } }));
  // The active-run shortcut must not keep the old branch after a navigation.
  agent.running.add(session);
  navigate();
  expect(host.openQuestions(session)).toEqual([]);
  expect(host.session(session)?.interactions.plan.markdown).toBe(undefined);
  await started;
  const refreshed = host.openSession(session);
  for (const frame of history("intermediate")) agent.push(frame);
  expect(host.session(session)?.interactions.plan.markdown).toBe(undefined);
  navigate();
  for (const frame of history("stale-tail")) agent.push(frame);
  expect(host.openQuestions(session)).toEqual([]);
  expect(host.session(session)?.interactions.plan.markdown).toBe(undefined);
  agent.replays.set(session, history("latest"));
  agent.respond(resumeId!, {});
  await refreshed;
  expect(host.openQuestions(session).map(({ key }) => key.requestId)).toEqual(["latest", "latest", "latest"]);
  expect(host.session(session)?.interactions.plan.markdown).toBe("latest");
  expect(host.session(session)?.tools.size).toBe(1);
  expect(host.running(session)).toBe(true);
});

test("plan_content_replays_replaces_by_id_and_is_shown_before_the_permission_response", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  const host = createHostStore(LOCAL_HOST, agent.transport);
  /** Makes a whole-replacement ACP plan frame for live and replay paths. */
  const plan = (data: unknown) => agent.update(session, { sessionUpdate: "plan_update", plan: data });
  agent.replays.set(session, [
    plan({ planId: "plan-mode", type: "markdown", content: "# Approved" }),
    plan({ planId: "todos", type: "items", entries: [{ content: "Old", status: "pending" }] }),
    plan({ planId: "todos", type: "items", entries: [{ content: "Build", status: "in_progress" }] }),
  ]);
  await host.start();
  await host.openSession(session);
  expect(host.session(session)?.interactions.plan).toEqual({ markdown: "# Approved", todos: [{ content: "Build", status: "in_progress" }] });
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "run_end", run_id: "first" } }));
  expect(host.session(session)?.interactions.plan.markdown).toBe("# Approved");
  agent.push(plan({ planId: "todos", type: "items", entries: [] }));
  expect(host.session(session)?.interactions.plan).toEqual({ markdown: "# Approved", todos: [] });
  const response = agent.requestPermission({ sessionId: session, title: "Execute plan?", subject: { type: "tool_call",
    toolCall: { kind: "switch_mode", content: [{ type: "content", content: { type: "text", text: "## Proposed" } }] } },
    options: [{ optionId: "approve", kind: "allow_once", name: "Approve" }] });
  expect(host.permissionFor(session)?.plan).toBe("## Proposed");
  host.answerPermission(host.permissionFor(session)!.key, "approve");
  expect(await response).toEqual({ outcome: { outcome: "selected", optionId: "approve" } });
});

test("reconnect_replays_changed_questions_and_refreshes_reversible_queue_reservations", async () => {
  const ask = { sessionUpdate: "tool_call_update", toolCallId: "tool", status: "completed", rawOutput: { content: { details: questions } } };
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  agent.replays.set(session, [agent.update(session, ask)]);
  const first = createHostStore(LOCAL_HOST, agent.transport);
  await first.start();
  await first.openSession(session);
  agent.onPrompt = () => {};
  const pending = first.answerQuestion(session, { requestId: "call-7", index: 0 }, "SQLite");
  agent.transport.end("disconnected");
  await expect(pending).rejects.toThrow("disconnected");
  expect(first.openQuestions(session).length).toBe(3);
  await expect(first.dismissQuestions(session, [first.openQuestions(session)[0]!.key])).rejects.toThrow();

  const next = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  next.replays.set(session, [next.update(session, ask), next.update(session, { sessionUpdate: "_clawcode/event",
    payload: { event: "message_end", message: reply(0) } })]);
  const send = next.transport.send.bind(next.transport);
  next.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (frame.method !== "_clawcode/session/pending_messages") return send(line);
    if (frame.params.questionsOnly) {
      next.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: frame.id, error: { code: -32602, message: "Unknown field" } }));
    } else next.respond(frame.id, { steering: [{ message: reply(1) }], followUp: [] });
  };
  const second = createHostStore(LOCAL_HOST, next.transport);
  await second.start(first);
  await second.openSession(session);
  expect(second.openQuestions(session).map((item) => item.key.index)).toEqual([2]);
  next.push(next.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "queue_changed", pending: { steering: [], followUp: [] } } }));
  expect(second.openQuestions(session).map((item) => item.key.index)).toEqual([1, 2]);
});

test("failed_branch_runtime_and_partial_resumes_remain_recoverable_on_the_current_session", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp", title: "Project" }]);
  agent.replays.set(session, [agent.userMessage(session, "old", "Previous branch")]);
  const host = createHostStore(LOCAL_HOST, agent.transport);
  await host.start();
  await host.openSession(session);
  agent.running.add(session);
  const send = agent.transport.send.bind(agent.transport);
  let failRuntime = true;
  let failResume = true;
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (failRuntime && frame.method === "_clawcode/session/runtime") {
      failRuntime = false;
      agent.fail(frame.id, "runtime unavailable");
    } else if (failResume && frame.method === "session/resume") {
      agent.push(agent.userMessage(session, "partial", "Interrupted history"));
      agent.fail(frame.id, "resume failed");
    } else send(line);
  };
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "branch_navigated" } }));
  await expect(host.openSession(session)).rejects.toThrow("resume failed");
  expect(host.notices().length).toBe(1);
  agent.push(agent.userMessage(session, "live", "Late frame"));
  expect(host.session(session)?.transcript.length).toBe(0);
  failResume = false;
  agent.replays.set(session, [agent.userMessage(session, "new", "Complete branch"), agent.userMessage(session, "live", "Late frame")]);
  await host.openSession(session);
  expect([...host.session(session)!.messages.values()].map((message) => message.text)).toEqual(["Complete branch", "Late frame"]);
  expect(host.running(session)).toBe(true);
});

test("reselecting_a_session_recovers_its_history_after_reservation_retries_are_exhausted", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp", title: "Project" }]);
  agent.running.add(session);
  agent.replays.set(session, [agent.userMessage(session, "first", "Original prompt"), agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: "ask",
    rawOutput: { content: { details: questions } } })]);
  const host = createHostStore(LOCAL_HOST, agent.transport);
  const app = createAppStore([host]);
  await app.start();
  const send = agent.transport.send.bind(agent.transport);
  let queries = 0;
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (frame.method !== "_clawcode/session/pending_messages") return send(line);
    queries++;
    expect(frame.params.questionsOnly).toBe(true);
    if (queries === 1) agent.respond(frame.id, { reservedQuestions: [{ requestId: "missing-index" }] });
    else if (queries === 2) agent.respond(frame.id, { steering: [{}], followUp: [] });
    else agent.respond(frame.id, { reservedQuestions: [{ requestId: "call-7", index: 1 }] });
  };
  app.openSession(LOCAL_HOST.id, session);
  await expect(host.openSession(session)).rejects.toThrow("Invalid question reservation snapshot");
  expect(queries).toBe(2);
  expect(agent.calls("session/resume").length).toBe(1);
  expect(host.notices().map(({ sessionId, description }) => [sessionId, !!description])).toEqual([[session, true]]);
  agent.push(agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: "late",
    rawOutput: { content: { details: questions } } }));
  expect(host.openQuestions(session)).toEqual([]);

  // The selected sidebar row remains pressable and shares the replay with other callers.
  app.openSession(LOCAL_HOST.id, session);
  await host.openSession(session);
  expect(queries).toBe(3);
  expect([...host.session(session)!.messages.values()].map(({ text }) => text)).toEqual(["Original prompt"]);
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([0, 2]);
  agent.push(agent.update(session, { sessionUpdate: "_clawcode/event", payload: { event: "queue_changed",
    pending: { steering: [], followUp: [], reservedQuestions: [{ requestId: "call-7", index: 2 }] } } }));
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([0, 1]);
});

test("a_timed_out_question_snapshot_retries_without_replaying_the_transcript_or_sending_a_reply", async () => {
  const agent = new ScriptedAgent([{ sessionId: session, cwd: "/tmp" }]);
  agent.replays.set(session, [agent.update(session, { sessionUpdate: "tool_call_update", toolCallId: "ask",
    rawOutput: { content: { details: questions } } })]);
  const host = createHostStore(LOCAL_HOST, agent.transport, { requestTimeoutMs: 20 });
  await host.start();
  const send = agent.transport.send.bind(agent.transport);
  let dropped: number | undefined;
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (frame.method !== "_clawcode/session/pending_messages") return send(line);
    if (dropped === undefined) dropped = frame.id;
    else agent.respond(frame.id, { reservedQuestions: [{ requestId: "call-7", index: 1 }] });
  };
  const opening = host.openSession(session);
  const stale = expect(host.answerQuestion(session, { requestId: "call-7", index: 1 }, "Already reserved")).rejects.toThrow();
  await Promise.all([opening, stale]);
  agent.respond(dropped!, { reservedQuestions: [] });
  expect(agent.calls("session/resume").length).toBe(1);
  expect(agent.calls("session/prompt").length).toBe(0);
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([0, 2]);
  expect(host.notices().length).toBe(0);
});

test("reconnect_withdraws_unverified_background_questions_until_a_fresh_ask_or_replay", async () => {
  const visible = "visible" as SessionId;
  const listed = [{ sessionId: visible, cwd: "/tmp" }, { sessionId: session, cwd: "/tmp" }];
  const first = new ScriptedAgent(listed);
  const ask = { sessionUpdate: "tool_call_update", toolCallId: "ask", rawOutput: { content: { details: questions } } };
  first.replays.set(session, [first.userMessage(session, "before", "Cached transcript"), first.update(session, ask)]);
  const previous = createHostStore(LOCAL_HOST, first.transport);
  const app = createAppStore([previous]);
  await app.start();
  await previous.openSession(session);
  app.openSession(LOCAL_HOST.id, visible);
  await previous.openSession(visible);
  expect(previous.awaitsAnswer(session)).toBe(true);
  first.transport.end("disconnected");

  const next = new ScriptedAgent(listed);
  next.replays.set(session, [next.update(session, ask), next.update(session, { sessionUpdate: "_clawcode/event",
    payload: { event: "message_end", message: reply(0) } })]);
  const host = createHostStore(LOCAL_HOST, next.transport);
  app.addHost(host);
  await host.start();
  await host.openSession(visible);
  expect(host.awaitsAnswer(session)).toBe(false);
  expect(host.openQuestions(session)).toEqual([]);
  expect(host.session(session)?.transcript).toBe(previous.session(session)?.transcript);
  expect(next.calls("session/resume").map(({ params }) => params.sessionId)).toEqual([visible]);

  // A new live ask restores attention without resurrecting the previous wire's unanswered keys.
  next.push(next.update(session, { ...ask, toolCallId: "new", rawOutput: { content: { details: {
    ...questions, request_id: "fresh", questions: [{ title: "New question" }],
  } } } }));
  expect(host.openQuestions(session).map(({ key }) => key.requestId)).toEqual(["fresh"]);
  expect(host.awaitsAnswer(session)).toBe(true);
  app.openSession(LOCAL_HOST.id, session);
  await expect(host.answerQuestion(session, { requestId: "call-7", index: 0 }, "Answered elsewhere")).rejects.toThrow();
  expect(host.openQuestions(session).map(({ key }) => key.index)).toEqual([1, 2]);
  expect(next.calls("session/resume").map(({ params }) => params.sessionId)).toEqual([visible, session]);
  expect(next.calls("session/prompt").length).toBe(0);
});

test("reconnect_keeps_a_new_question_delivered_before_the_session_directory_reply", async () => {
  const listed = [{ sessionId: session, cwd: "/tmp" }];
  const first = new ScriptedAgent(listed);
  const ask = { sessionUpdate: "tool_call_update", toolCallId: "ask", rawOutput: { content: { details: questions } } };
  first.replays.set(session, [first.userMessage(session, "history", "Cached transcript"), first.update(session, ask)]);
  const previous = createHostStore(LOCAL_HOST, first.transport);
  await previous.start();
  await previous.openSession(session);
  first.transport.end("disconnected");

  for (const handshake of ["ready", "rejected", "closed"]) {
    const next = new ScriptedAgent(listed);
    const host = createHostStore(LOCAL_HOST, next.transport);
    const send = next.transport.send.bind(next.transport);
    const listing = new Promise<string>((resolve) => {
      next.transport.send = (line) => {
        if (JSON.parse(line).method === "session/list") resolve(line);
        else send(line);
      };
    });
    const started = host.start(previous);
    const line = await listing;
    next.push(next.update(session, { ...ask, toolCallId: "new", rawOutput: { content: { details: {
      ...questions, request_id: "during-handshake", questions: [{ title: "New question" }],
    } } } }));
    // The last connected Session remains readable while the directory request is outstanding.
    const pendingHistory = host.session(session)?.transcript;
    if (handshake === "rejected") next.fail(JSON.parse(line).id, "Directory unavailable");
    else if (handshake === "closed") {
      next.respond(JSON.parse(line).id, { sessions: [] });
      next.transport.end("Connection lost after directory response");
    } else send(line);
    await started;
    expect(next.calls("session/resume").length).toBe(0);
    if (handshake !== "ready") {
      expect(host.phase().kind).toBe("offline");
      expect(host.sessions()).toEqual(previous.sessions());
      expect(host.openQuestions(session)).toEqual(previous.openQuestions(session));
      expect(host.session(session)?.transcript).toBe(previous.session(session)?.transcript);
    } else {
      expect(host.phase().kind).toBe("ready");
      expect(host.openQuestions(session).map(({ key }) => key.requestId)).toEqual(["during-handshake"]);
      expect(host.awaitsAnswer(session)).toBe(true);
    }
    expect(pendingHistory).toBe(previous.session(session)?.transcript);
  }
});
