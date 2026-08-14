export type Brand<T, Name extends string> = T & { readonly __brand: Name };

export type SessionId = Brand<string, "SessionId">;
export type TurnId = Brand<string, "TurnId">;
export type MessageId = Brand<string, "MessageId">;
export type ToolCallId = Brand<string, "ToolCallId">;
export type QueueId = Brand<string, "QueueId">;
export type EntryId = Brand<string, "EntryId">;
export type TimestampMs = Brand<string, "TimestampMs">;

export type JsonRpcError = Readonly<{
  code: number;
  message: string;
  data?: unknown;
}>;

export type JsonRpcMessage =
  | Readonly<{ jsonrpc: "2.0"; id: number; result: unknown }>
  | Readonly<{ jsonrpc: "2.0"; id: number; error: JsonRpcError }>
  | Readonly<{ jsonrpc: "2.0"; id: number; method: string; params: unknown }>
  | Readonly<{ jsonrpc: "2.0"; method: string; params: unknown }>;

export type EventMeta = Readonly<{
  turnId: TurnId;
  timestampMs: TimestampMs;
  sequence: number;
}>;

export type AcpMeta = Readonly<Record<string, unknown>>;

export type InitializeResult = Readonly<{
  protocolVersion: number;
  info: Readonly<{ name: string; title?: string; version: string }>;
  capabilities: Readonly<Record<string, unknown> & {
    session?: Readonly<Record<string, unknown> & { delete?: Readonly<Record<string, unknown>> | null }> | null;
  }>;
  _meta?: AcpMeta;
}>;

export type SessionInfo = Readonly<{
  sessionId: SessionId;
  cwd: string;
  title?: string | null;
  updatedAt?: string | null;
  _meta?: AcpMeta;
}>;

export type SessionListResult = Readonly<{
  sessions: readonly SessionInfo[];
  nextCursor?: string | null;
}>;

export type NewSessionResult = Readonly<{ sessionId: SessionId }>;

export type TextContentBlock = Readonly<{ type: "text"; text: string }>;
export type ResourceLinkContentBlock = Readonly<{
  type: "resource_link";
  name: string;
  uri: string;
}>;
export type PromptContentBlock = TextContentBlock | ResourceLinkContentBlock;

export type SessionUpdateNotification = Readonly<{
  sessionId: SessionId;
  update: Readonly<Record<string, unknown>>;
  _meta?: AcpMeta;
}>;

export const AcpProtocol = {
  methods: {
    initialize: "initialize",
    sessionNew: "session/new",
    sessionList: "session/list",
    sessionResume: "session/resume",
    sessionClose: "session/close",
    sessionDelete: "session/delete",
    sessionPrompt: "session/prompt",
    sessionCancel: "session/cancel",
    sessionUpdate: "session/update"
  },
  decodeRecord(value: unknown, label: string): Record<string, unknown> {
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      throw new Error(`${label} must be an object`);
    }
    return value as Record<string, unknown>;
  },
  /** Returns only the configured product namespace from ACP `_meta`. */
  productMeta(meta: unknown, namespace: string): Record<string, unknown> | undefined {
    if (typeof meta !== "object" || meta === null || Array.isArray(meta)) {
      return undefined;
    }
    const namespaced = (meta as Record<string, unknown>)[namespace];
    return typeof namespaced === "object" && namespaced !== null && !Array.isArray(namespaced)
      ? namespaced as Record<string, unknown>
      : undefined;
  },
  eventMeta(meta: unknown, namespace: string): EventMeta | undefined {
    const value = this.productMeta(meta, namespace);
    if (value === undefined) return undefined;
    if (
      typeof value.turnId !== "string" ||
      typeof value.timestampMs !== "string" ||
      !/^\d+$/.test(value.timestampMs) ||
      typeof value.sequence !== "number"
    ) {
      return undefined;
    }
    return {
      turnId: value.turnId as TurnId,
      timestampMs: value.timestampMs as TimestampMs,
      sequence: value.sequence
    };
  }
} as const;
