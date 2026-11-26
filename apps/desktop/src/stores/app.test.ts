import { expect, test } from "nsh/test";

import type { SessionId } from "../client/acp/protocol";
import { acpPeer, MemoryTransport } from "../../tests/support/memoryTransport";
import { ScriptedAgent } from "../../tests/support/scriptedAgent";
import { createAppStore } from "./app";
import { createHostStore } from "./host";
import type { HostStore } from "./host";

/** A host `id` whose transport answers, or fails to open with `openError`. */
function host(id: string, openError?: string): HostStore {
  return createHostStore({ id, name: id.toUpperCase(), kind: "remote" }, new MemoryTransport(acpPeer([]), openError));
}

test("selecting_a_host_navigates_and_back_returns", () => {
  const store = createAppStore([host("a"), host("b")]);
  expect(store.location()).toEqual({ hostId: "a", sessionId: null });
  store.selectHost("b");
  expect(store.currentHost().info.id).toBe("b");
  store.back();
  expect(store.currentHost().info.id).toBe("a");
  expect(store.canGoForward()).toBe(true);
});

test("opening_a_session_on_another_host_switches_current_host", () => {
  const store = createAppStore([host("a"), host("b")]);
  store.openSession("b", "s1" as SessionId);
  expect(store.currentHost().info.id).toBe("b");
  expect(store.location().sessionId).toBe("s1");
});

test("one_host_failing_leaves_others_ready", async () => {
  const store = createAppStore([host("a"), host("b", "unreachable")]);
  await store.start();
  const [a, b] = store.hosts();
  expect(a?.phase().kind).toBe("ready");
  expect(b?.phase().kind).toBe("failed");
});

test("unknown_host_keeps_the_location", () => {
  const store = createAppStore([host("a")]);
  store.selectHost("nope");
  store.openSession("nope", "s1" as SessionId);
  expect(store.location()).toEqual({ hostId: "a", sessionId: null });
});

test("retrying_a_remote_host_replaces_only_that_connection", async () => {
  const first = host("a");
  const failed = host("b", "unreachable");
  const store = createAppStore([first, failed]);
  await store.start();
  store.selectHost("b");
  const connected = host("b");
  store.addHost(connected);
  await connected.start();
  expect(store.hosts().length).toBe(2);
  expect(store.currentHost()).toBe(connected);
  expect(connected.phase().kind).toBe("ready");
  expect(first.phase().kind).toBe("ready");
  store.stop();
});

test("failed_retry_preserves_the_offline_directory_and_transcript_throughout_the_attempt", async () => {
  const agent = new ScriptedAgent([{ sessionId: "A", cwd: "/work", title: "Conversation A" }]);
  agent.replays.set("A", [agent.agentMessage("A", "reply", "Cached answer")]);
  const old = createHostStore({ id: "remote", name: "remote", kind: "remote" }, agent.transport);
  const app = createAppStore([old]);
  const id = "A" as SessionId;
  await app.start();
  app.openSession("remote", id);
  await old.openSession(id);
  agent.transport.end("offline");

  const retry = host("remote", "unavailable");
  app.addHost(retry);
  expect(app.currentHost().phase().kind).toBe("reconnecting");
  expect(app.currentHost().sessions()[0]?.title).toBe("Conversation A");
  expect(app.currentHost().session(id)?.transcript.length).toBe(1);
  await retry.start();
  expect(app.currentHost().phase().kind).toBe("offline");
  expect(app.currentHost().sessions()[0]?.title).toBe("Conversation A");
  expect(app.currentHost().session(id)).toEqual(old.session(id));
  app.stop();
});

test("history_replays_the_session_on_the_new_connection_after_a_background_retry", async () => {
  const listing = [{ sessionId: "A", cwd: "/work", title: "Conversation A" }];
  const first = new ScriptedAgent(listing);
  const second = new ScriptedAgent(listing);
  second.replays.set("A", [second.agentMessage("A", "fresh", "After reconnect")]);
  const info = { id: "remote", name: "remote", kind: "remote" } as const;
  const previous = createHostStore(info, first.transport);
  const app = createAppStore([host("local"), previous]);
  const id = "A" as SessionId;
  await app.start();
  app.openSession("remote", id);
  await previous.openSession(id);
  first.transport.end("offline");
  const retry = createHostStore(info, second.transport);
  app.addHost(retry);
  app.selectHost("local");
  await retry.start();
  app.back();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(second.calls("session/resume").length).toBe(1);
  expect(app.currentHost().session(id)?.transcript.length).toBe(1);
  app.back();
  app.forward();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(second.calls("session/resume").length).toBe(2);
  app.stop();
});
