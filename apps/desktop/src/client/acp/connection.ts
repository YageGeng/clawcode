import type { AcpTransport } from "../transport";
import { messageOf } from "../errors";
import { AcpProtocol } from "./protocol";
import type { JsonRpcError, JsonRpcMessage } from "./protocol";

/** A timer handle on any host: browsers return numbers, Node and NSH may return objects. */
type Timer = ReturnType<typeof setTimeout>;

type PendingRequest = Readonly<{
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
  timeout?: Timer;
}>;

export type AcpNotification = Readonly<{
  method: string;
  params: unknown;
}>;

/** Answers one agent-to-client request; throwing `AcpRequestError` replies with its code. */
export type AcpRequestHandler = (params: unknown, signal: AbortSignal) => Promise<unknown>;

export type AcpConnectionCallbacks = Readonly<{
  notification: (notification: AcpNotification) => void;
  diagnostic: (message: string) => void;
  closed: (reason: string) => void;
}>;

export class AcpRequestError extends Error {
  readonly rpc: JsonRpcError;

  constructor(rpc: JsonRpcError) {
    super(`ACP ${rpc.code}: ${rpc.message}`);
    this.name = "AcpRequestError";
    this.rpc = rpc;
  }
}

export class AcpConnection {
  readonly transport: AcpTransport;
  readonly callbacks: AcpConnectionCallbacks;
  private started = false;
  private opened = false;
  private requestId = 0;
  private readonly pending = new Map<number, PendingRequest>();
  private readonly handlers = new Map<string, AcpRequestHandler>();
  // Agent requests still being answered, aborted by `$/cancel_request` or close.
  private readonly inbound = new Map<number | string, AbortController>();
  private explicitlyClosed = false;

  /** Creates a connection over `transport`; nothing is sent until `connect`. */
  constructor(transport: AcpTransport, callbacks: AcpConnectionCallbacks) {
    this.transport = transport;
    this.callbacks = callbacks;
  }

  /** Opens the transport once, failing after `timeoutMs`. */
  connect(timeoutMs = 10_000): Promise<void> {
    if (this.started) {
      throw new Error("ACP connection already started");
    }
    this.started = true;
    this.explicitlyClosed = false;
    const transport = this.transport;
    transport.onFrame((frame) => this.receive(frame));
    let settled = false;
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        if (settled) return;
        settled = true;
        transport.close();
        reject(new Error("ACP connection timed out"));
      }, timeoutMs);
      transport.onClose((reason) => {
        clearTimeout(timeout);
        const message = reason ?? "ACP connection closed";
        this.opened = false;
        if (!settled) {
          settled = true;
          reject(new Error(message));
        }
        this.rejectPending(new Error(message));
        this.abortInbound();
        if (!this.explicitlyClosed) {
          this.callbacks.closed(message);
        }
      });
      transport.open().then(() => {
        clearTimeout(timeout);
        if (settled) return;
        settled = true;
        this.opened = true;
        resolve();
      }, (reason: unknown) => {
        clearTimeout(timeout);
        if (settled) return;
        settled = true;
        reject(reason instanceof Error ? reason : new Error(String(reason)));
      });
    });
  }

  /** Sends one ACP request and reports an unavailable transport through its Promise. */
  request<T>(
    method: string,
    params: unknown,
    options: Readonly<{ timeoutMs?: number }> = {}
  ): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      try {
        this.assertOpen();
      } catch (reason: unknown) {
        reject(reason instanceof Error ? reason : new Error(String(reason)));
        return;
      }
      const id = this.nextRequestId();
      const timeout = options.timeoutMs === undefined ? undefined : setTimeout(() => {
        if (!this.pending.delete(id)) return;
        reject(new Error(`ACP request ${method} timed out`));
      }, options.timeoutMs);
      this.pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
        ...(timeout === undefined ? {} : { timeout })
      });
      try {
        this.transport.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      } catch (reason: unknown) {
        const pending = this.pending.get(id);
        if (pending?.timeout !== undefined) clearTimeout(pending.timeout);
        this.pending.delete(id);
        reject(reason instanceof Error ? reason : new Error(String(reason)));
      }
    });
  }

  /** Registers the handler for one agent-to-client request method. */
  handle(method: string, handler: AcpRequestHandler): void {
    this.handlers.set(method, handler);
  }

  /** Sends one notification; throws when the transport is not open. */
  notify(method: string, params: unknown): void {
    this.assertOpen();
    this.transport.send(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  close(): void {
    this.explicitlyClosed = true;
    this.rejectPending(new Error("ACP connection closed"));
    this.abortInbound();
    this.opened = false;
    this.transport.close();
  }

  /** Throws unless the transport is open. */
  private assertOpen(): void {
    if (!this.opened) {
      throw new Error("ACP connection is not open");
    }
  }

  private nextRequestId(): number {
    if (this.requestId >= Number.MAX_SAFE_INTEGER) {
      this.requestId = 0;
    }
    do {
      this.requestId += 1;
    } while (this.pending.has(this.requestId));
    return this.requestId;
  }

  /** Decodes one single-message JSON-RPC transport frame; ACP v2 forbids batches. */
  private receive(data: string): void {
    let message: JsonRpcMessage;
    try {
      const value: unknown = JSON.parse(data);
      if (Array.isArray(value)) {
        this.callbacks.diagnostic("Ignored JSON-RPC batch frame; ACP v2 forbids batches");
        return;
      }
      const record = AcpProtocol.decodeRecord(value, "JSON-RPC message");
      if (record.jsonrpc !== "2.0") {
        throw new Error("JSON-RPC version must be 2.0");
      }
      message = record as JsonRpcMessage;
    } catch (reason: unknown) {
      this.callbacks.diagnostic(messageOf(reason));
      return;
    }
    if ("id" in message && !("method" in message)) {
      const pending = this.pending.get(message.id);
      if (pending === undefined) {
        this.callbacks.diagnostic(`Ignored response for unknown id ${message.id}`);
        return;
      }
      this.pending.delete(message.id);
      if (pending.timeout !== undefined) clearTimeout(pending.timeout);
      if ("error" in message) {
        pending.reject(new AcpRequestError(message.error));
      } else {
        pending.resolve(message.result);
      }
      return;
    }
    if ("method" in message && "id" in message) {
      this.answer(message.id, message.method, message.params);
      return;
    }
    if ("method" in message && message.method === "$/cancel_request") {
      const params = message.params as Readonly<{ requestId?: number | string }> | undefined;
      if (params?.requestId !== undefined) this.inbound.get(params.requestId)?.abort();
      return;
    }
    // One frame carries one notification; a negotiated batch carrier is
    // expanded later by the host, which knows the product namespace.
    if ("method" in message) {
      this.callbacks.notification({ method: message.method, params: message.params });
    }
  }

  /** Runs a registered handler and replies once; unknown methods get -32601. */
  private answer(id: number | string, method: string, params: unknown): void {
    const reply = (body: Readonly<{ result: unknown }> | Readonly<{ error: JsonRpcError }>) => {
      // A closed connection has nobody left to answer.
      if (!this.opened) return;
      try {
        this.transport.send(JSON.stringify({ jsonrpc: "2.0", id, ...body }));
      } catch (reason: unknown) {
        // A transport that is closing (a WebSocket in CLOSING) has nobody to answer either.
        this.callbacks.diagnostic(`Dropped the reply to ${method}: ${messageOf(reason)}`);
      }
    };
    const handler = this.handlers.get(method);
    if (handler === undefined) {
      reply({ error: { code: -32601, message: `Method not found: ${method}` } });
      return;
    }
    const controller = new AbortController();
    this.inbound.set(id, controller);
    handler(params, controller.signal)
      .then((result) => reply({ result }))
      .catch((reason: unknown) => reply({
        error: reason instanceof AcpRequestError
          ? reason.rpc
          : { code: -32603, message: messageOf(reason) }
      }))
      .finally(() => this.inbound.delete(id));
  }

  /** Abandons agent requests whose connection is gone. */
  private abortInbound(): void {
    for (const controller of this.inbound.values()) controller.abort();
    this.inbound.clear();
  }

  private rejectPending(error: Error): void {
    for (const request of this.pending.values()) {
      if (request.timeout !== undefined) clearTimeout(request.timeout);
      request.reject(error);
    }
    this.pending.clear();
  }
}
