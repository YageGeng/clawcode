import { expect, test } from "nsh/test";

import type { SessionId, SessionInfo } from "../../src/client/acp/protocol";
import { acpPeer, MemoryTransport } from "../support/memoryTransport";
import { createHostStore, LOCAL_HOST } from "../../src/stores/host";

/** Two listed Sessions. */
const sessions = [
  { sessionId: "s1", cwd: "/work/a", title: "First" },
  { sessionId: "s2", cwd: "/work/b", title: "Second" },
] as unknown as SessionInfo[];

test("host_lists_sessions_after_start", async () => {
  const host = createHostStore(LOCAL_HOST, new MemoryTransport(acpPeer(sessions)));
  await host.start();
  expect(host.phase().kind).toBe("ready");
  expect(host.sessions().length).toBe(2);
});

test("host_reports_a_transport_that_cannot_open", async () => {
  const host = createHostStore(LOCAL_HOST, new MemoryTransport(() => {}, "config file not found"));
  await host.start();
  expect(host.phase()).toEqual({ kind: "failed", failure: { cause: "start", detail: "config file not found" } });
});

test("starting_twice_keeps_a_ready_host", async () => {
  const host = createHostStore(LOCAL_HOST, new MemoryTransport(acpPeer(sessions)));
  await Promise.all([host.start(), host.start()]);
  await host.start();
  expect(host.phase().kind).toBe("ready");
});

test("host_times_out_when_backend_never_answers", async () => {
  const host = createHostStore(LOCAL_HOST, new MemoryTransport(() => {}), { requestTimeoutMs: 50 });
  await host.start();
  const phase = host.phase();
  expect(phase.kind).toBe("failed");
  expect(phase.kind === "failed" && phase.failure.cause).toBe("unanswered");
});

test("diff_selections_are_scoped_to_host_and_session_and_survive_a_reconnect", async () => {
  const local = createHostStore(LOCAL_HOST, new MemoryTransport(acpPeer(sessions)));
  const info = { id: "paired", kind: "remote", name: "Paired" } as const;
  const remote = createHostStore(info, new MemoryTransport(acpPeer(sessions)));
  const id = sessions[0]!.sessionId;
  const selection = { tab: "files", change: { turnKey: "turn-1", path: "src/a.rs", editId: "second-edit" } } as const;
  await remote.start();
  remote.setPanel(id, selection);
  expect(local.panels().get(id)).toBe(undefined);
  expect(remote.panels().get(sessions[1]!.sessionId)).toBe(undefined);
  const reconnected = createHostStore(info, new MemoryTransport(acpPeer(sessions)));
  await reconnected.start(remote);
  expect(reconnected.panels().get(id)).toEqual(selection);
  reconnected.setPanel(id);
  expect(reconnected.panels().has(id)).toBe(false);
});

test("queryRunning reads runtime without attaching the session", async () => {
  const peer = acpPeer(sessions);
  const host = createHostStore(LOCAL_HOST, new MemoryTransport((frame, transport) => {
    const request = JSON.parse(frame) as { id?: number; method?: string; params?: { sessionId?: string } };
    if (request.method === "_clawcode/session/runtime") {
      transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { running: request.params?.sessionId === "s2" } }));
      return;
    }
    peer(frame, transport);
  }));
  await host.start();
  expect(await host.queryRunning("s2" as SessionId)).toBe(true);
  expect(await host.queryRunning("s1" as SessionId)).toBe(false);
  expect(host.session("s2" as SessionId)).toBe(undefined);
});

test("host_reads_the_default_session_settings_for_new_drafts", async () => {
  const peer = acpPeer(sessions);
  const host = createHostStore(LOCAL_HOST, new MemoryTransport((frame, transport) => {
    const request = JSON.parse(frame) as { id?: number; method?: string };
    if (request.method === "_clawcode/session/default_config") {
      transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result: [
        { configId: "model", type: "select", name: "Model", currentValue: "p/flash",
          options: [{ value: "p/flash", name: "DeepSeek V4 Flash" }] },
      ] }));
      return;
    }
    peer(frame, transport);
  }));
  await host.start();
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(host.defaultConfig().find((option) => option.id === "model")?.currentValue).toBe("p/flash");
});

/** A host whose peer also answers `session/set_title` and `session/delete`, recording the requests it saw. */
async function managedHost() {
  const seen: Array<{ method: string; params: Record<string, unknown> }> = [];
  let listed = [...sessions];
  const host = createHostStore(LOCAL_HOST, new MemoryTransport((frame, transport) => {
    const request = JSON.parse(frame) as { id?: number; method?: string; params?: Record<string, unknown> };
    const reply = (result: unknown) => transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }));
    if (request.method === "session/set_title") {
      seen.push({ method: request.method, params: request.params ?? {} });
      return reply({ title: request.params?.title });
    }
    if (request.method === "session/delete") {
      seen.push({ method: request.method, params: request.params ?? {} });
      listed = listed.filter((session) => session.sessionId !== request.params?.sessionId);
      return reply({});
    }
    if (request.method === "session/list") return reply({ sessions: listed });
    acpPeer(listed)(frame, transport);
  }));
  await host.start();
  return { host, seen };
}

test("renaming_a_session_retitles_it_on_the_host_and_in_the_list", async () => {
  const { host, seen } = await managedHost();
  await host.renameSession("s1" as SessionId, "  Parser work  ");
  expect(seen).toEqual([{ method: "session/set_title", params: { sessionId: "s1", title: "Parser work" } }]);
  expect(host.sessions().find((session) => session.sessionId === "s1")?.title).toBe("Parser work");
});

test("deleting_a_session_removes_it_from_the_list", async () => {
  const { host, seen } = await managedHost();
  await host.deleteSession("s2" as SessionId);
  expect(seen).toEqual([{ method: "session/delete", params: { sessionId: "s2" } }]);
  expect(host.sessions().map((session) => session.sessionId)).toEqual(["s1"]);
});
