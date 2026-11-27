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
