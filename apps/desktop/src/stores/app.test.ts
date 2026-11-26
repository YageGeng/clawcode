import { expect, test } from "nsh/test";

import type { SessionId } from "../client/acp/protocol";
import { acpPeer, MemoryTransport } from "../../tests/support/memoryTransport";
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
