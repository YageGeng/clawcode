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

test("host_reads_sign_in_methods_and_their_statuses", async () => {
  const peer = acpPeer(sessions);
  const host = createHostStore(LOCAL_HOST, new MemoryTransport((frame, transport) => {
    const request = JSON.parse(frame) as { id?: number; method?: string };
    const reply = (result: unknown) => transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }));
    if (request.method === "initialize") {
      return reply({ protocolVersion: 2, authMethods: [
        // ACP v2's wire shape, as the backend sends it.
        { type: "agent", methodId: "openai-codex", name: "ChatGPT", description: "Sign in with your ChatGPT plan" },
        { id: "broken" },
      ] });
    }
    if (request.method === "_clawcode/auth/status") {
      return reply([{ methodId: "openai-codex", status: "signInRequired" }, { methodId: "copilot", status: "weird" }]);
    }
    peer(frame, transport);
  }));
  await host.start();
  // A method without a name is not one the user can pick.
  expect(host.authMethods()).toEqual([{ id: "openai-codex", name: "ChatGPT", description: "Sign in with your ChatGPT plan" }]);
  // An unknown status reads as undecided rather than failing the whole list.
  expect(await host.signInStatuses()).toEqual([
    { methodId: "openai-codex", status: "signInRequired" }, { methodId: "copilot", status: "unknown" },
  ]);
});

/** A host whose peer runs `auth/login` through a URL elicitation, recording what it receives. */
async function signInHost() {
  const received: Array<Record<string, unknown>> = [];
  let loginId: number | undefined;
  let transportRef: MemoryTransport | undefined;
  const peer = acpPeer(sessions);
  const host = createHostStore(LOCAL_HOST, new MemoryTransport((frame, transport) => {
    transportRef = transport;
    const message = JSON.parse(frame) as Record<string, unknown> & { id?: number; method?: string };
    received.push(message);
    const reply = (result: unknown) => transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: message.id, result }));
    if (message.method === "initialize") {
      return reply({ protocolVersion: 2, authMethods: [{ type: "agent", methodId: "openai-codex", name: "ChatGPT" }] });
    }
    if (message.method === "auth/login") {
      loginId = message.id;
      transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: "agent-1", method: "elicitation/create", params: {
        mode: "url", requestId: message.id, elicitationId: "e1", url: "https://example.com/device",
        message: "Open the page and enter ABCD-1234", _meta: { clawcode: { userCode: "ABCD-1234", methodName: "ChatGPT" } },
      } }));
      return;
    }
    if (message.method === "session/prompt") {
      return transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: message.id,
        error: { code: -32000, message: "Authentication required", data: { methodId: "openai-codex" } } }));
    }
    peer(frame, transport);
  }));
  await host.start();
  /** Finishes the sign-in as the backend does: withdraw the device code, then answer `auth/login`. */
  const complete = () => {
    transportRef!.deliver(JSON.stringify({ jsonrpc: "2.0", method: "$/cancel_request", params: { requestId: "agent-1" } }));
    transportRef!.deliver(JSON.stringify({ jsonrpc: "2.0", id: loginId, result: {} }));
  };
  return { host, received, complete, loginId: () => loginId, deliver: (message: unknown) => transportRef!.deliver(JSON.stringify(message)) };
}

test("initialize_offers_url_elicitation_for_sign_in", async () => {
  const { received } = await signInHost();
  const init = received.find((message) => message.method === "initialize") as { params: { capabilities: { elicitation?: unknown } } };
  expect(init.params.capabilities.elicitation).toEqual({ url: {} });
});

test("a_sign_in_shows_its_device_code_until_the_backend_finishes", async () => {
  const { host, complete } = await signInHost();
  const done = host.signIn("openai-codex");
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(host.signInFlow()).toEqual({ methodId: "openai-codex", code: {
    url: "https://example.com/device", userCode: "ABCD-1234", message: "Open the page and enter ABCD-1234",
  } });
  complete();
  await done;
  expect(host.signInFlow()).toBe(null);
});

test("cancelling_a_sign_in_declines_the_code_and_cancels_the_login", async () => {
  const { host, received, loginId } = await signInHost();
  const done = host.signIn("openai-codex").catch(() => "cancelled");
  await new Promise((resolve) => setTimeout(resolve, 0));
  host.cancelSignIn();
  expect(await done).toBe("cancelled");
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(received.find((message) => message.id === "agent-1")?.result).toEqual({ action: "decline" });
  expect((received.find((message) => message.method === "$/cancel_request") as { params: { requestId: number } }).params.requestId)
    .toBe(loginId()!);
  expect(host.signInFlow()).toBe(null);
});

test("withdrawing_an_old_device_code_keeps_the_fresh_one", async () => {
  const { host, received, deliver, loginId } = await signInHost();
  const done = host.signIn("openai-codex").catch(() => "cancelled");
  await new Promise((resolve) => setTimeout(resolve, 0));
  // The host issues a fresh code before it withdraws the expired one.
  deliver({ jsonrpc: "2.0", id: "agent-2", method: "elicitation/create", params: {
    mode: "url", requestId: loginId(), elicitationId: "e2", url: "https://example.com/device2", message: "Enter WXYZ-9876", _meta: { clawcode: { userCode: "WXYZ-9876" } },
  } });
  deliver({ jsonrpc: "2.0", method: "$/cancel_request", params: { requestId: "agent-1" } });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(host.signInFlow()?.code?.userCode).toBe("WXYZ-9876");
  host.cancelSignIn();
  expect(await done).toBe("cancelled");
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(received.find((message) => message.id === "agent-2")?.result).toEqual({ action: "decline" });
});

/** Unrelated or malformed scopes must never replace the login's device card or be declined as its code. */
test("a_sign_in_cancels_unrelated_url_elicitations_without_claiming_them", async () => {
  const { host, received, deliver, loginId } = await signInHost();
  const done = host.signIn("openai-codex").catch(() => "cancelled");
  const code = host.signInFlow()?.code;
  const scopes = [
    { requestId: loginId()! + 1 },
    { requestId: String(loginId()) },
    { sessionId: "s1", toolCallId: "tool-1" },
    { sessionId: "s1", requestId: loginId() },
    { requestId: loginId(), toolCallId: "tool-1" },
    {},
  ];
  for (const [index, scope] of scopes.entries()) {
    const id = `unrelated-${index}`;
    deliver({ jsonrpc: "2.0", id, method: "elicitation/create", params: {
      mode: "url", ...scope, elicitationId: id, url: "https://example.com/unrelated", message: "An unrelated tool request",
    } });
  }
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(host.signInFlow()?.code).toEqual(code);
  host.cancelSignIn();
  expect(await done).toBe("cancelled");
  await new Promise((resolve) => setTimeout(resolve, 0));
  for (const index of scopes.keys()) {
    expect(received.find((message) => message.id === `unrelated-${index}`)?.result).toEqual({ action: "cancel" });
  }
  expect(received.find((message) => message.id === "agent-1")?.result).toEqual({ action: "decline" });
});

/** Late elicitations from a cancelled login cannot become a retry's device code. */
test("retrying_sign_in_rejects_the_previous_login_scope", async () => {
  const { host, received, deliver, loginId, complete } = await signInHost();
  const first = host.signIn("openai-codex").catch(() => "cancelled");
  const oldId = loginId();
  host.cancelSignIn();
  // Cancellation revokes ownership immediately, before the login Promise's continuation.
  deliver({ jsonrpc: "2.0", id: "after-cancel", method: "elicitation/create", params: {
    mode: "url", requestId: oldId, elicitationId: "late-cancel", url: "https://example.com/old",
  } });
  expect(await first).toBe("cancelled");
  const retry = host.signIn("openai-codex");
  expect(loginId() === oldId).toBe(false);
  deliver({ jsonrpc: "2.0", id: "old-login", method: "elicitation/create", params: {
    mode: "url", requestId: oldId, elicitationId: "old", url: "https://example.com/old",
  } });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(host.signInFlow()?.code?.userCode).toBe("ABCD-1234");
  expect(received.find((message) => message.id === "after-cancel")?.result).toEqual({ action: "cancel" });
  expect(received.find((message) => message.id === "old-login")?.result).toEqual({ action: "cancel" });
  complete();
  await retry;
  expect(host.signInFlow()).toBe(null);
});

/** A response ends request ownership in wire order, before the async login caller resumes. */
test("a_completed_sign_in_rejects_late_elicitations", async () => {
  const { host, received, deliver, loginId, complete } = await signInHost();
  const done = host.signIn("openai-codex");
  complete();
  deliver({ jsonrpc: "2.0", id: "after-completion", method: "elicitation/create", params: {
    mode: "url", requestId: loginId(), elicitationId: "late", url: "https://example.com/late",
  } });
  await done;
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(received.find((message) => message.id === "after-completion")?.result).toEqual({ action: "cancel" });
  expect(host.signInFlow()).toBe(null);
});

test("a_prompt_that_needs_sign_in_names_the_method_in_its_notice", async () => {
  const { host } = await signInHost();
  await host.prompt("s1" as SessionId, "hello");
  const notice = host.notices().at(-1)!;
  expect(notice.signIn).toBe("openai-codex");
  expect(notice.title).toBe("Sign in to ChatGPT to continue");
});

/** Opens a host whose `session/prompt` first reports a failed Run, then answers as `respond` says. */
async function failingPromptHost(respond: (id: number, transport: MemoryTransport) => void) {
  const peer = acpPeer(sessions);
  const host = createHostStore(LOCAL_HOST, new MemoryTransport((frame, transport) => {
    const message = JSON.parse(frame) as { id: number; method?: string };
    if (message.method !== "session/prompt") return peer(frame, transport);
    // The kernel settles a Run that failed before insertion, then the insertion channel refuses the prompt.
    transport.deliver(JSON.stringify({ jsonrpc: "2.0", method: "session/update", params: {
      sessionId: "s1",
      update: { sessionUpdate: "state_update", state: "idle", stopReason: "error", error: { code: -32603, message: "store failed" } },
      _meta: { clawcode: { turnId: "t1", timestampMs: "1", sequence: 1 } },
    } }));
    respond(message.id, transport);
  }));
  await host.start();
  return host;
}

test("a_run_failed_before_insertion_raises_one_notice", async () => {
  const host = await failingPromptHost((id, transport) => transport.deliver(JSON.stringify({
    jsonrpc: "2.0", id, error: { code: -32603, message: "store failed" },
  })));
  await host.prompt("s1" as SessionId, "hello");
  // Only the refused prompt speaks, and it hands the draft back.
  expect(host.notices().map((notice) => notice.title)).toEqual(["The message was not sent"]);
  expect(host.returnedDrafts().has("s1" as SessionId)).toBe(true);
});

test("a_run_that_fails_after_its_prompt_was_accepted_still_raises_its_notice", async () => {
  const host = await failingPromptHost((id, transport) => transport.deliver(JSON.stringify({ jsonrpc: "2.0", id, result: { userMessageId: "m1" } })));
  await host.prompt("s1" as SessionId, "hello");
  expect(host.notices().map((notice) => notice.title)).toEqual(["The agent stopped with an error"]);
});
