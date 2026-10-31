import { AcpProtocol } from "./protocol";
import type { JsonRpcError, JsonRpcMessage } from "./protocol";

type PendingRequest = Readonly<{
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
  timeout?: number;
}>;

export type AcpNotification = Readonly<{
  method: string;
  params: unknown;
}>;

/** Answers one agent-to-client request; throwing `AcpRequestError` replies with its code. */
export type AcpRequestHandler = (params: unknown, signal: AbortSignal) => Promise<unknown>;

export type AcpConnectionCallbacks = Readonly<{
  notifications: (notifications: readonly AcpNotification[]) => void;
  diagnostic: (message: string) => void;
  closed: (reason: string) => void;
  activity: () => void;
}>;

export class AcpRequestError extends Error {
  readonly rpc: JsonRpcError;

  constructor(rpc: JsonRpcError) {
    super(`ACP ${rpc.code}: ${rpc.message}`);
    this.name = "AcpRequestError";
    this.rpc = rpc;
  }

  /** Matches the server's exact missing-session payload, never a generic internal or network error. */
  isSessionNotFound(sessionId: string): boolean {
    return this.rpc.code === -32603 && this.rpc.data === `session not found: ${sessionId}`;
  }
}

export class AcpConnection {
  readonly url: URL;
  readonly callbacks: AcpConnectionCallbacks;
  private socket: WebSocket | undefined;
  private requestId = 0;
  private readonly pending = new Map<number, PendingRequest>();
  private readonly handlers = new Map<string, AcpRequestHandler>();
  // Agent requests still being answered, aborted by `$/cancel_request` or close.
  private readonly inbound = new Map<number | string, AbortController>();
  private explicitlyClosed = false;

  constructor(url: URL, callbacks: AcpConnectionCallbacks) {
    this.url = url;
    this.callbacks = callbacks;
  }

  connect(timeoutMs = 10_000): Promise<void> {
    if (this.socket !== undefined) {
      throw new Error("ACP connection already started");
    }
    this.explicitlyClosed = false;
    const socket = new WebSocket(this.url);
    this.socket = socket;
    return new Promise((resolve, reject) => {
      let settled = false;
      const timeout = window.setTimeout(() => {
        if (settled) return;
        settled = true;
        socket.close(1000, "connection timeout");
        reject(new Error("ACP WebSocket connection timed out"));
      }, timeoutMs);
      socket.addEventListener("open", () => {
        if (settled) return;
        settled = true;
        window.clearTimeout(timeout);
        resolve();
      }, { once: true });
      socket.addEventListener("error", () => {
        if (!settled && socket.readyState !== WebSocket.OPEN) {
          settled = true;
          window.clearTimeout(timeout);
          reject(new Error("ACP WebSocket failed to open"));
        }
      }, { once: true });
      socket.addEventListener("message", (event) => this.receive(event.data));
      socket.addEventListener("close", (event) => {
        window.clearTimeout(timeout);
        const reason = event.reason || `WebSocket closed with code ${event.code}`;
        if (!settled) {
          settled = true;
          reject(new Error(reason));
        }
        this.rejectPending(new Error(reason));
        this.abortInbound();
        if (this.socket === socket) this.socket = undefined;
        if (!this.explicitlyClosed) {
          this.callbacks.closed(reason);
        }
      });
    });
  }

  /** Sends one ACP request and reports unavailable sockets through its Promise. */
  request<T>(
    method: string,
    params: unknown,
    options: Readonly<{ timeoutMs?: number }> = {}
  ): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      let socket: WebSocket;
      try {
        socket = this.openSocket();
      } catch (reason: unknown) {
        reject(reason instanceof Error ? reason : new Error(String(reason)));
        return;
      }
      const id = this.nextRequestId();
      const timeout = options.timeoutMs === undefined ? undefined : window.setTimeout(() => {
        if (!this.pending.delete(id)) return;
        reject(new Error(`ACP request ${method} timed out`));
      }, options.timeoutMs);
      this.pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
        ...(timeout === undefined ? {} : { timeout })
      });
      try {
        socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      } catch (reason: unknown) {
        const pending = this.pending.get(id);
        if (pending?.timeout !== undefined) window.clearTimeout(pending.timeout);
        this.pending.delete(id);
        reject(reason instanceof Error ? reason : new Error(String(reason)));
      }
    });
  }

  /** Registers the handler for one agent-to-client request method. */
  handle(method: string, handler: AcpRequestHandler): void {
    this.handlers.set(method, handler);
  }

  notify(method: string, params: unknown): void {
    this.openSocket().send(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  close(): void {
    this.explicitlyClosed = true;
    this.rejectPending(new Error("ACP connection closed"));
    this.abortInbound();
    this.socket?.close(1000, "client closed");
    this.socket = undefined;
  }

  private openSocket(): WebSocket {
    if (this.socket?.readyState !== WebSocket.OPEN) {
      throw new Error("ACP WebSocket is not open");
    }
    return this.socket;
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

  /** Decodes one JSON-RPC transport frame and preserves its notification boundary. */
  private receive(data: unknown): void {
    if (typeof data !== "string") {
      this.callbacks.diagnostic("Ignored non-text ACP WebSocket frame");
      return;
    }
    let messages: JsonRpcMessage[];
    try {
      const value: unknown = JSON.parse(data);
      const values = Array.isArray(value) ? value : [value];
      if (values.length === 0) throw new Error("JSON-RPC batch must not be empty");
      messages = values.map((item, index) => {
        const record = AcpProtocol.decodeRecord(
          item,
          values.length === 1 ? "JSON-RPC message" : `JSON-RPC batch member ${index}`
        );
        if (record.jsonrpc !== "2.0") {
          throw new Error("JSON-RPC version must be 2.0");
        }
        return record as JsonRpcMessage;
      });
    } catch (reason: unknown) {
      this.callbacks.diagnostic(
        reason instanceof Error ? reason.message : String(reason)
      );
      return;
    }
    this.callbacks.activity();
    const notifications: AcpNotification[] = [];
    for (const message of messages) {
      if ("id" in message && !("method" in message)) {
        const pending = this.pending.get(message.id);
        if (pending === undefined) {
          this.callbacks.diagnostic(`Ignored response for unknown id ${message.id}`);
          continue;
        }
        this.pending.delete(message.id);
        if (pending.timeout !== undefined) window.clearTimeout(pending.timeout);
        if ("error" in message) {
          pending.reject(new AcpRequestError(message.error));
        } else {
          pending.resolve(message.result);
        }
        continue;
      }
      if ("method" in message && "id" in message) {
        this.answer(message.id, message.method, message.params);
        continue;
      }
      if ("method" in message && message.method === "$/cancel_request") {
        const params = message.params as Readonly<{ requestId?: number | string }> | undefined;
        if (params?.requestId !== undefined) this.inbound.get(params.requestId)?.abort();
        continue;
      }
      if ("method" in message) {
        notifications.push({ method: message.method, params: message.params });
      }
    }
    if (notifications.length > 0) this.callbacks.notifications(notifications);
  }

  /** Runs a registered handler and replies once; unknown methods get -32601. */
  private answer(id: number | string, method: string, params: unknown): void {
    const reply = (body: Readonly<{ result: unknown }> | Readonly<{ error: JsonRpcError }>) => {
      // A closed connection has nobody left to answer.
      const socket = this.socket;
      if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ jsonrpc: "2.0", id, ...body }));
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
          : { code: -32603, message: reason instanceof Error ? reason.message : String(reason) }
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
      if (request.timeout !== undefined) window.clearTimeout(request.timeout);
      request.reject(error);
    }
    this.pending.clear();
  }
}
