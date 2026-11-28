import { expect, test } from "nsh/test";

import { AcpConnection } from "../../src/client/acp/connection";
import type { AcpConnectionCallbacks } from "../../src/client/acp/connection";
import { MemoryTransport } from "../support/memoryTransport";

/** Callbacks that record diagnostics and closes and ignore the rest. */
function recording(): AcpConnectionCallbacks & { diagnostics: string[]; closes: string[] } {
  const diagnostics: string[] = [];
  const closes: string[] = [];
  return {
    diagnostics,
    closes,
    notification: () => {},
    diagnostic: (message) => diagnostics.push(message),
    closed: (reason) => closes.push(reason),
  };
}

test("connection_answers_a_request_through_the_transport", async () => {
  const transport = new MemoryTransport((frame, self) => {
    const request = JSON.parse(frame) as { id: number; method: string };
    expect(request.method).toBe("initialize");
    self.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { protocolVersion: 2 } }));
  });
  const connection = new AcpConnection(transport, recording());
  await connection.connect();
  const result = await connection.request<{ protocolVersion: number }>("initialize", { protocolVersion: 2 });
  expect(result).toEqual({ protocolVersion: 2 });
});

/** Correlation is available before a synchronous transport can deliver the peer's response. */
test("a_request_reports_its_id_before_sending", async () => {
  let observedId: number | undefined;
  let requestId: number | undefined;
  let idAtSend: number | undefined;
  const transport = new MemoryTransport((frame, self) => {
    const request = JSON.parse(frame) as { id: number };
    requestId = request.id;
    idAtSend = observedId;
    self.deliver(JSON.stringify({ jsonrpc: "2.0", id: request.id, result: {} }));
  });
  const connection = new AcpConnection(transport, recording());
  await connection.connect();
  await connection.request("auth/login", {}, { onRequestId: (id) => { observedId = id; } });
  expect(typeof requestId).toBe("number");
  expect(idAtSend).toBe(requestId);
});

test("connection_rejects_pending_requests_when_the_transport_closes", async () => {
  const callbacks = recording();
  const transport = new MemoryTransport((_frame, self) => self.end("backend stopped"));
  const connection = new AcpConnection(transport, callbacks);
  await connection.connect();
  await expect(connection.request("session/list", {})).rejects.toThrow("backend stopped");
  expect(callbacks.closes).toEqual(["backend stopped"]);
});

test("reply_on_a_closing_transport_is_dropped_with_a_diagnostic", async () => {
  // The peer asks a question; by the time the answer is ready the transport
  // is closing and every send throws.
  let closing = false;
  const callbacks = recording();
  const transport = new MemoryTransport(() => {
    if (closing) throw new Error("transport is closing");
  });
  const connection = new AcpConnection(transport, callbacks);
  connection.handle("session/request_permission", async () => {
    closing = true;
    return { outcome: "cancelled" };
  });
  await connection.connect();
  transport.deliver(JSON.stringify({ jsonrpc: "2.0", id: 7, method: "session/request_permission", params: {} }));
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(callbacks.diagnostics).toEqual(["Dropped the reply to session/request_permission: transport is closing"]);
});

test("an_aborted_request_cancels_on_the_peer_and_rejects", async () => {
  const sent: Array<{ method?: string; id?: number; params?: { requestId?: number } }> = [];
  const transport = new MemoryTransport((frame, self) => {
    const message = JSON.parse(frame) as { id?: number; method?: string; params?: { requestId?: number } };
    sent.push(message);
    if (message.method === "initialize") self.deliver(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {} }));
  });
  const connection = new AcpConnection(transport, recording());
  await connection.connect();
  const controller = new AbortController();
  const login = connection.request("auth/login", { methodId: "m" }, { signal: controller.signal });
  controller.abort();
  let rejected = false;
  await login.catch(() => { rejected = true; });
  expect(rejected).toBe(true);
  const request = sent.find((message) => message.method === "auth/login")!;
  expect(sent.find((message) => message.method === "$/cancel_request")?.params?.requestId).toBe(request.id);
});

test("an_already_aborted_signal_rejects_without_sending", async () => {
  const sent: string[] = [];
  const transport = new MemoryTransport((frame) => { sent.push(frame); });
  const connection = new AcpConnection(transport, recording());
  await connection.connect();
  const controller = new AbortController();
  controller.abort();
  await expect(connection.request("auth/login", {}, { signal: controller.signal })).rejects.toThrow("cancelled");
  expect(sent.length).toBe(0);
});

test("a_request_answered_inside_send_leaves_no_abort_listener", async () => {
  const transport = new MemoryTransport((frame, self) => {
    const message = JSON.parse(frame) as { id: number };
    self.deliver(JSON.stringify({ jsonrpc: "2.0", id: message.id, result: {} }));
  });
  const connection = new AcpConnection(transport, recording());
  await connection.connect();
  // Listeners still attached, in order of adding and removing.
  const attached = new Set<unknown>();
  const signal = {
    aborted: false,
    addEventListener: (_type: string, listener: unknown) => attached.add(listener),
    removeEventListener: (_type: string, listener: unknown) => attached.delete(listener),
  } as unknown as AbortSignal;
  await connection.request("session/list", {}, { signal });
  expect(attached.size).toBe(0);
});
