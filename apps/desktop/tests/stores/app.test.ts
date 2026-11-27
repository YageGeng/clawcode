import { expect, test } from "nsh/test";

import type { SessionId } from "../../src/client/acp/protocol";
import { acpPeer, MemoryTransport } from "../support/memoryTransport";
import { ScriptedAgent } from "../support/scriptedAgent";
import { createAppStore } from "../../src/stores/app";
import { createHostStore } from "../../src/stores/host";
import type { HostStore } from "../../src/stores/host";

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

test("the_first_prompt_reuses_the_session_prepared_on_the_selected_host", async () => {
  const local = new ScriptedAgent();
  const remote = new ScriptedAgent();
  const localHost = createHostStore({ id: "local", name: "Local", kind: "local" }, local.transport);
  const remoteHost = createHostStore({ id: "remote", name: "Remote", kind: "remote" }, remote.transport);
  const app = createAppStore([localHost, remoteHost]);
  await app.start();
  app.selectHost("remote");
  const id = await remoteHost.newSession("/selected");
  const image = { id: "local-id", name: "private-name.png", mimeType: "image/png", size: 2, data: "YWI=" };
  await app.startSession("remote", { text: "Use my settings", images: [image] }, { session: async () => id, canOpen: () => true });
  expect(app.location()).toEqual({ hostId: "remote", sessionId: id });
  expect(remote.calls("session/new").length).toBe(1);
  expect(remote.calls("session/prompt")[0]?.params.sessionId).toBe(id);
  expect(remote.calls("session/prompt")[0]?.params.prompt).toEqual([
    { type: "text", text: "Use my settings" }, { type: "image", mimeType: "image/png", data: image.data },
  ]);
  expect(local.calls("session/prompt").length).toBe(0);
  app.stop();
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
