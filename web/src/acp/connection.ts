import { AcpProtocol } from "./protocol";
import type { JsonRpcError, JsonRpcMessage } from "./protocol";

type PendingRequest = Readonly<{
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
}>;

export type AcpConnectionCallbacks = Readonly<{
  notification: (method: string, params: unknown) => void;
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
  readonly url: URL;
  readonly callbacks: AcpConnectionCallbacks;
  private socket: WebSocket | undefined;
  private requestId = 0;
  private readonly pending = new Map<number, PendingRequest>();
  private explicitlyClosed = false;

  constructor(url: URL, callbacks: AcpConnectionCallbacks) {
    this.url = url;
    this.callbacks = callbacks;
  }

  connect(): Promise<void> {
    if (this.socket !== undefined) {
      throw new Error("ACP connection already started");
    }
    this.explicitlyClosed = false;
    const socket = new WebSocket(this.url);
    this.socket = socket;
    return new Promise((resolve, reject) => {
      socket.addEventListener("open", () => resolve(), { once: true });
      socket.addEventListener("error", () => {
        if (socket.readyState !== WebSocket.OPEN) {
          reject(new Error("ACP WebSocket failed to open"));
        }
      }, { once: true });
      socket.addEventListener("message", (event) => this.receive(event.data));
      socket.addEventListener("close", (event) => {
        const reason = event.reason || `WebSocket closed with code ${event.code}`;
        this.rejectPending(new Error(reason));
        this.socket = undefined;
        if (!this.explicitlyClosed) {
          this.callbacks.closed(reason);
        }
      });
    });
  }

  request<T>(method: string, params: unknown): Promise<T> {
    const socket = this.openSocket();
    const id = this.nextRequestId();
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject
      });
      try {
        socket.send(JSON.stringify({ jsonrpc: "2.0", id, method, params }));
      } catch (reason: unknown) {
        this.pending.delete(id);
        reject(reason instanceof Error ? reason : new Error(String(reason)));
      }
    });
  }

  notify(method: string, params: unknown): void {
    this.openSocket().send(JSON.stringify({ jsonrpc: "2.0", method, params }));
  }

  close(): void {
    this.explicitlyClosed = true;
    this.rejectPending(new Error("ACP connection closed"));
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

  private receive(data: unknown): void {
    if (typeof data !== "string") {
      this.callbacks.diagnostic("Ignored non-text ACP WebSocket frame");
      return;
    }
    let message: JsonRpcMessage;
    try {
      const value: unknown = JSON.parse(data);
      const record = AcpProtocol.decodeRecord(value, "JSON-RPC message");
      if (record.jsonrpc !== "2.0") {
        throw new Error("JSON-RPC version must be 2.0");
      }
      message = record as JsonRpcMessage;
    } catch (reason: unknown) {
      this.callbacks.diagnostic(
        reason instanceof Error ? reason.message : String(reason)
      );
      return;
    }
    if ("id" in message && !("method" in message)) {
      const pending = this.pending.get(message.id);
      if (pending === undefined) {
        this.callbacks.diagnostic(`Ignored response for unknown id ${message.id}`);
        return;
      }
      this.pending.delete(message.id);
      if ("error" in message) {
        pending.reject(new AcpRequestError(message.error));
      } else {
        pending.resolve(message.result);
      }
      return;
    }
    if ("method" in message && "id" in message) {
      this.openSocket().send(JSON.stringify({
        jsonrpc: "2.0",
        id: message.id,
        error: { code: -32601, message: "Client callbacks are not supported" }
      }));
      return;
    }
    if ("method" in message) {
      this.callbacks.notification(message.method, message.params);
    }
  }

  private rejectPending(error: Error): void {
    for (const request of this.pending.values()) {
      request.reject(error);
    }
    this.pending.clear();
  }
}
