// The host as a Session controller, driven by a scripted ACP agent (no model).
import { expect, test } from "nsh/test";

import type { SessionId } from "../client/acp/protocol";
import { ScriptedAgent } from "../../tests/support/scriptedAgent";
import { createHostStore, LOCAL_HOST } from "./host";
import type { HostStore } from "./host";

const s1 = "s1" as SessionId;

/** A started host over `agent`. */
async function hostOver(agent: ScriptedAgent): Promise<HostStore> {
  const host = createHostStore(LOCAL_HOST, agent.transport);
  await host.start();
  return host;
}

/** Waits until `condition` holds, polling across turns of the event loop. */
async function until(condition: () => boolean, timeoutMs = 2000): Promise<void> {
  const started = Date.now();
  while (!condition()) {
    if (Date.now() - started > timeoutMs) throw new Error("condition never held");
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/** The transcript length of Session `id` on `host`. */
function transcript(host: HostStore, id: SessionId): number {
  return host.workspace().sessionWorkspaces.get(id)?.transcript.length ?? 0;
}

test("opening_a_session_replays_its_transcript", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a", title: "First" }]);
  agent.replays.set("s1", [agent.userMessage("s1", "u1", "hi"), agent.agentMessage("s1", "a1", "hello")]);
  const host = await hostOver(agent);
  await host.openSession(s1);
  expect(transcript(host, s1)).toBe(2);
});

test("a_partial_replay_cannot_replace_the_last_complete_transcript_when_the_link_fails", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  agent.replays.set("s1", [agent.userMessage("s1", "u1", "Original")]);
  const host = await hostOver(agent);
  await host.openSession(s1);
  const cached = host.session(s1);
  const send = agent.transport.send.bind(agent.transport);
  let replayStarted = false;
  agent.transport.send = (line) => {
    if (JSON.parse(line).method === "session/resume") {
      agent.push(agent.userMessage("s1", "partial", "Partial replay"));
      replayStarted = true;
    } else {
      send(line);
    }
  };
  const replay = host.openSession(s1).then(() => "unexpected success", () => "interrupted");
  await until(() => replayStarted);
  expect(host.session(s1)).toEqual(cached);
  agent.transport.end("offline");
  expect(await replay).toBe("interrupted");
  expect(host.session(s1)).toEqual(cached);
});

test("replay_publishes_history_once_and_keeps_live_run_state_visible", async () => {
  const s2 = "s2" as SessionId;
  const agent = new ScriptedAgent([{ sessionId: s1, cwd: "/work" }, { sessionId: s2, cwd: "/other" }]);
  agent.replays.set(s1, [agent.userMessage(s1, "saved", "Original")]);
  const host = await hostOver(agent);
  await host.openSession(s1);
  const savedHistory = host.session(s1)!.transcript;
  const directory = host.workspace().sessionWorkspaces;

  // Hold just the resume reply so live state and partial history arrive on the same wire.
  let requestId: number | undefined;
  const send = agent.transport.send.bind(agent.transport);
  agent.transport.send = (line) => {
    const frame = JSON.parse(line);
    if (frame.method === "session/resume") requestId = frame.id;
    else send(line);
  };
  const replay = host.openSession(s1);
  await until(() => requestId !== undefined);
  agent.push(agent.userMessage(s1, "replacement", "New history"));
  expect(host.workspace().sessionWorkspaces).toBe(directory);
  agent.push(agent.state(s1, "running"));
  expect(host.running(s1)).toBe(true);
  expect(host.session(s1)!.running).toBe(true);
  expect(host.session(s1)!.transcript).toBe(savedHistory);
  agent.push(agent.agentMessage(s2, "another", "An independent live message"));
  expect(host.session(s2)!.transcript.length).toBe(1);
  agent.respond(requestId!, {});
  await replay;
  expect([...host.session(s1)!.messages.values()].map((message) => message.text)).toEqual(["New history"]);
  expect(host.running(s1)).toBe(true);
});

test("prompt_streams_a_reply", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  agent.onPrompt = (call) => {
    agent.push(agent.state("s1", "running"));
    agent.push(agent.userMessage("s1", "u1", call.text));
    for (const piece of ["Hel", "lo ", "there"]) agent.push(agent.agentChunk("s1", "a1", piece));
    agent.push(agent.state("s1", "idle"));
    agent.respond(call.id, { stopReason: "end_turn" });
  };
  const host = await hostOver(agent);
  await host.openSession(s1);
  await host.prompt(s1, "hi");
  const session = host.workspace().sessionWorkspaces.get(s1);
  expect(session?.messages.get("a1" as never)?.text).toBe("Hello there");
  expect(session?.running).toBe(false);
});

test("permission_request_waits_for_the_answer", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  const host = await hostOver(agent);
  const answer = agent.requestPermission({
    sessionId: "s1",
    title: "Write a.txt",
    options: [{ optionId: "allow-once", name: "Allow once", kind: "allow_once" }, { optionId: "reject-once", name: "Reject", kind: "reject_once" }],
  });
  await until(() => host.workspace().permissions.length === 1);
  host.answerPermission(host.workspace().permissions[0]!.key, "allow-once");
  expect(await answer).toEqual({ outcome: { outcome: "selected", optionId: "allow-once" } });
  expect(host.workspace().permissions.length).toBe(0);
});

test("permission_withdrawn_when_answered_elsewhere", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  const host = await hostOver(agent);
  let requestId = "";
  const answer = agent.requestPermission(
    { sessionId: "s1", title: "Write a.txt", options: [{ optionId: "allow-once", name: "Allow once", kind: "allow_once" }] },
    (id) => (requestId = id),
  );
  await until(() => host.workspace().permissions.length === 1);
  const key = host.workspace().permissions[0]!.key;
  agent.withdraw(requestId);
  await until(() => host.workspace().permissions.length === 0);
  expect(await answer).toEqual({ outcome: { outcome: "cancelled" } });
  const before = agent.received.length;
  host.answerPermission(key, "allow-once");
  expect(agent.received.length).toBe(before);
});

test("prompt_failure_returns_the_text_and_notifies", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  agent.onPrompt = (call) => agent.fail(call.id, "provider unavailable");
  const host = await hostOver(agent);
  await host.openSession(s1);
  await host.prompt(s1, "hello");
  expect(host.takeReturnedText(s1)).toBe("hello");
  expect(host.takeReturnedText(s1)).toBe(undefined);
  expect(host.workspace().notices.length).toBe(1);
  expect(host.workspace().sessionWorkspaces.get(s1)?.running ?? false).toBe(false);
});

test("disconnect_mid_turn_goes_offline_and_returns_the_text", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }, { sessionId: "s2", cwd: "/work/b" }]);
  // Both Sessions run when the backend goes away; s1's prompt never gets its answer.
  agent.onPrompt = (call) => {
    agent.push(agent.state("s1", "running"));
    agent.push(agent.state("s2", "running"));
    agent.push(agent.userMessage("s1", "u1", call.text));
  };
  const host = await hostOver(agent);
  await host.openSession(s1);
  const sending = host.prompt(s1, "hello");
  await until(() => host.workspace().sessionWorkspaces.get(s1)?.running === true);
  agent.transport.end("crashed");
  await sending;
  expect(host.phase().kind).toBe("offline");
  // No spinner keeps turning for a turn nothing will finish, and what was sent comes back to the composer.
  expect(host.workspace().sessionWorkspaces.get(s1)?.running ?? false).toBe(false);
  expect(host.workspace().sessionWorkspaces.get("s2" as SessionId)?.running ?? false).toBe(false);
  expect(host.takeReturnedText(s1)).toBe("hello");
  expect(host.workspace().notices.length).toBe(1);
});

test("cancel_sends_session_cancel", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  const host = await hostOver(agent);
  host.cancel(s1);
  await until(() => agent.calls("session/cancel").length === 1);
  expect(agent.calls("session/cancel")[0]!.params.sessionId).toBe("s1");
});

test("reopening_a_running_session_does_not_replay", async () => {
  const agent = new ScriptedAgent([{ sessionId: "s1", cwd: "/work/a" }]);
  agent.running.add("s1");
  const host = await hostOver(agent);
  await host.openSession(s1);
  await host.openSession(s1);
  expect(agent.calls("session/resume").length).toBe(1);
});

test("new_session_lists_and_opens_it", async () => {
  const agent = new ScriptedAgent();
  const host = await hostOver(agent);
  const id = await host.newSession("/work/new");
  expect(agent.calls("session/new")[0]!.params.cwd).toBe("/work/new");
  expect(host.sessions().map((session) => session.sessionId)).toEqual([id]);
  expect(agent.calls("session/resume").length).toBe(1);
});
