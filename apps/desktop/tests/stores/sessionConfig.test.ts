import { expect, test } from "nsh/test";

import { MemoryTransport } from "../support/memoryTransport";
import type { SessionId } from "../../src/client/acp/protocol";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";

const s1 = "s1" as SessionId;
const s2 = "s2" as SessionId;
const remote = { id: "peer", name: "Peer", kind: "remote" } as const;
const models = [{ value: "one/default", name: "Default model" }, { value: "two/org/model", name: "Other model" }];

/** The full ACP snapshot, including grouped select choices and a future option kind. */
function snapshot(model: string, mode = "default") {
  return { configOptions: [
    { configId: "model", category: "model", type: "select", name: "Model", currentValue: model,
      options: [{ groupId: "providers", name: "Providers", options: models }] },
    { configId: "thinking_level", type: "select", name: "Thinking", currentValue: "low", options: [{ value: "low", name: "Low" }] },
    { configId: "mode", type: "select", name: "Mode", currentValue: mode, options: [
      { value: "default", name: "Default" }, { value: "plan", name: "Plan" },
    ] },
    { configId: "future", type: "boolean", name: "Future", currentValue: true },
  ] };
}

/** A peer exposing the same RPC methods and response shapes as the production ACP backend. */
class ConfigPeer {
  resume: unknown = null;
  configRequest?: { id: number; params: Record<string, unknown> };
  writes = 0;

  readonly transport = new MemoryTransport((line) => {
    const { id, method, params } = JSON.parse(line);
    switch (method) {
      case "initialize": return this.answer(id, { protocolVersion: 2 });
      case "session/list": return this.answer(id, { sessions: [s1, s2].map((sessionId) => ({ sessionId, cwd: "/work" })) });
      case "_clawcode/session/runtime": return this.answer(id, { running: false });
      case "session/new": return this.answer(id, { sessionId: s1, ...snapshot("one/default") });
      case "session/resume": return this.answer(id, this.resume);
      case "session/set_config_option":
        this.writes++;
        this.configRequest = { id, params };
        return;
      default: throw new Error(`Unexpected request ${method}`);
    }
  });

  /** Delivers an RPC result immediately, including while another frame is being sent. */
  answer(id: number, result: unknown) {
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id, result }));
  }

  /** Publishes configuration on the same session/update path used for transcript data. */
  publish(sessionId: SessionId, config: unknown) {
    this.transport.deliver(JSON.stringify({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId, update: { sessionUpdate: "config_option_update", configOptions: config },
    } }));
  }
}

test("session_config_from_new_survives_replay_and_resume_refreshes_all_selects", async () => {
  const peer = new ConfigPeer();
  const host = createHostStore(LOCAL_HOST, peer.transport);
  await host.start();
  const created = await host.newSession("/work");
  expect(host.sessionConfig(created)[0]).toEqual({ id: "model", name: "Model", currentValue: "one/default", options: models });
  peer.resume = snapshot("two/org/model", "plan");
  await host.openSession(created);
  expect(host.sessionConfig(created).map((config) => [config.id, config.currentValue])).toEqual([
    ["model", "two/org/model"], ["thinking_level", "low"], ["mode", "plan"],
  ]);
});

test("session_config_writes_validate_choices_and_observe_responses_and_notifications_in_wire_order", async () => {
  const peer = new ConfigPeer();
  const host = createHostStore(remote, peer.transport);
  peer.resume = snapshot("one/default");
  await host.start();
  await host.openSession(s1);
  await expect(host.setSessionConfig(s1, "model", "unknown")).rejects.toThrow();
  expect(peer.writes).toBe(0);
  const pending = host.setSessionConfig(s1, "model", "two/org/model");
  expect(host.configPending(s1)).toBe(true);
  expect(host.configPending(s2)).toBe(false);
  expect(host.sessionConfig(s1)[0]!.currentValue).toBe("one/default");
  await expect(host.setSessionConfig(s1, "mode", "plan")).rejects.toThrow();
  expect(peer.writes).toBe(1);
  expect(peer.configRequest!.params).toEqual({ sessionId: s1, configId: "model", type: "id", value: "two/org/model" });
  peer.publish(s1, snapshot("one/default", "plan").configOptions);
  peer.answer(peer.configRequest!.id, snapshot("two/org/model", "plan"));
  await pending;
  expect(host.configPending(s1)).toBe(false);
  expect(host.sessionConfig(s1)[0]!.currentValue).toBe("two/org/model");

  const next = host.setSessionConfig(s1, "mode", "default");
  peer.answer(peer.configRequest!.id, snapshot("two/org/model"));
  // Both frames arrive before the Promise continuation for the response gets a turn.
  peer.publish(s1, snapshot("one/default", "plan").configOptions);
  await next;
  expect(host.sessionConfig(s1)[0]!.currentValue).toBe("one/default");
  expect(host.sessionConfig(s1)[2]!.currentValue).toBe("plan");
  expect(host.sessionConfig(s2)).toEqual([]);
});

test("session_config_preserves_confirmed_choices_on_a_malformed_update_and_accepts_an_empty_snapshot", async () => {
  const peer = new ConfigPeer();
  const host = createHostStore(LOCAL_HOST, peer.transport);
  await host.start();
  await host.newSession("/work");
  const confirmed = host.sessionConfig(s1);
  peer.publish(s1, snapshot("not-offered").configOptions);
  expect(host.sessionConfig(s1)).toBe(confirmed);
  peer.publish(s1, []);
  expect(host.sessionConfig(s1)).toEqual([]);
});

test("session_config_keeps_confirmed_values_on_errors_and_reconnects_only_within_its_host", async () => {
  const peer = new ConfigPeer();
  const host = createHostStore(remote, peer.transport);
  await host.start();
  await host.newSession("/work");
  const confirmed = host.sessionConfig(s1);
  const malformed = host.setSessionConfig(s1, "mode", "plan");
  peer.answer(peer.configRequest!.id, { configOptions: "bad response" });
  await expect(malformed).rejects.toThrow();
  expect(host.sessionConfig(s1)).toBe(confirmed);
  const failed = host.setSessionConfig(s1, "mode", "plan");
  peer.transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: peer.configRequest!.id, error: { code: -32000, message: "Sign in required" } }));
  await expect(failed).rejects.toThrow("Sign in required");
  expect(host.sessionConfig(s1)).toBe(confirmed);
  expect(host.configPending(s1)).toBe(false);
  const dropped = host.setSessionConfig(s1, "mode", "plan");
  peer.transport.end("host disconnected");
  await expect(dropped).rejects.toThrow("host disconnected");
  await expect(host.setSessionConfig(s1, "mode", "plan")).rejects.toThrow();
  expect(peer.writes).toBe(3);

  const local = createHostStore(LOCAL_HOST, new ConfigPeer().transport);
  await local.start(host);
  expect(local.sessionConfig(s1)).toEqual([]);
  const replacement = createHostStore(remote, new ConfigPeer().transport);
  await replacement.start(host);
  expect(replacement.configPending(s1)).toBe(false);
  expect(replacement.sessionConfig(s1)).toBe(confirmed);
  await replacement.openSession(s1);
  expect(replacement.sessionConfig(s1)).toBe(confirmed);
});
