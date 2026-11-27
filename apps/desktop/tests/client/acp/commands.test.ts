import { expect, test } from "nsh/test";

import { MemoryTransport } from "../../support/memoryTransport";
import { createHostStore, LOCAL_HOST } from "../../../src/stores/host";
import { decodeCommands } from "../../../src/client/acp/commands";
import type { SessionId } from "../../../src/client/acp/protocol";

test("available_commands_decodes_only_supported_entries_in_order_and_preserves_invalid_envelopes", () => {
  expect(decodeCommands(undefined)).toBe(undefined);
  expect(decodeCommands([])).toEqual([]);
  expect(decodeCommands([
    { type: "future", name: "hidden" },
    { type: "command", name: "session", description: "Session details" },
    { type: "command", name: "session", description: "Duplicate" },
    { type: "command", name: "bad", description: "", input: { type: "future" } },
    { type: "command", name: "name", description: "Rename", input: { type: "text", hint: "<title>" } },
  ])).toEqual([{ name: "session", description: "Session details" }, { name: "name", description: "Rename", hint: "<title>" }]);
});

test("available_commands_new_resume_and_live_snapshots_are_isolated_per_host_and_session", async () => {
  const id = "same-id" as SessionId;
  const snapshot = (name: string) => ({ availableCommands: [{ type: "command", name, description: name }] });
  let resume: unknown = null;
  const transport: MemoryTransport = new MemoryTransport((line) => {
    const req = JSON.parse(line);
    const result = req.method === "session/new" ? { sessionId: id, ...snapshot("created") }
      : req.method === "session/resume" ? resume
      : req.method === "session/list" ? { sessions: [{ sessionId: id, cwd: "/project" }] }
      : req.method === "initialize" ? { protocolVersion: 2 } : { running: false };
    transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: req.id, result }));
  });
  const local = createHostStore(LOCAL_HOST, transport);
  const remote = createHostStore({ id: "peer", name: "Peer", kind: "remote" }, new MemoryTransport(() => undefined));
  await local.start();
  await local.newSession("/project");
  expect(local.commandsFor(id)[0]?.name).toBe("created");
  resume = snapshot("resumed");
  await local.openSession(id);
  expect(local.commandsFor(id)[0]?.name).toBe("resumed");
  for (const value of [snapshot("live"), { availableCommands: null }]) {
    transport.deliver(JSON.stringify({ jsonrpc: "2.0", method: "session/update", params: { sessionId: id,
      update: { sessionUpdate: "available_commands_update", ...value } } }));
  }
  expect(local.commandsFor(id)[0]?.name).toBe("live");
  expect(local.commandsFor("another" as SessionId)).toEqual([]);
  expect(remote.commandsFor(id)).toEqual([]);
  local.stop();
});
