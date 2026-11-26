import { expect, test } from "nsh/test";

import type { SessionInfo } from "../client/acp/protocol";
import { acpPeer, MemoryTransport } from "../../tests/support/memoryTransport";
import { createHostStore, LOCAL_HOST } from "./host";

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
