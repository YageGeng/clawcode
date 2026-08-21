export type Brand<T, Name extends string> = T & { readonly __brand: Name };

export type SessionId = Brand<string, "SessionId">;
export type RunId = Brand<string, "RunId">;
export type EventSequence = Brand<number, "EventSequence">;
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
    session?: Readonly<Record<string, unknown> & {
      delete?: Readonly<Record<string, unknown>> | null;
      prompt?: Readonly<Record<string, unknown> & { image?: Readonly<Record<string, unknown>> | null }> | null;
    }> | null;
  }>;
  _meta?: AcpMeta;
}>;

export type SessionRecoveryCursor = Readonly<{
  sessionId: SessionId;
  runId: RunId;
  nextSequence: EventSequence;
}>;

export type SessionRecoveryPlan = Readonly<{
  sessionId: SessionId;
  mode: "watch" | "resume";
  runId?: RunId;
  nextSequence?: EventSequence;
  running?: boolean;
}>;

export type InitializeRecovery = Readonly<{
  version: 1;
  sessions: readonly SessionRecoveryPlan[];
}>;

export type ProjectionGroupMeta = Readonly<{
  runId: RunId;
  projectionIndex: number;
  projectionCount: number;
  lastSequence?: EventSequence;
  operationPhase?: "start" | "end";
}>;

export type ResumeRecoveryMeta = Readonly<{
  mode: "watch" | "resume";
  runId?: RunId;
  journalTailSequence?: EventSequence;
  running: boolean;
}>;

export type CursorUnavailableData = Readonly<{
  code: "_clawcode/session_recovery_cursor_unavailable";
  sessionId: SessionId;
  runId: RunId;
  sequence: EventSequence;
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
export type ImageMimeType = "image/png" | "image/jpeg" | "image/gif" | "image/webp";
export type ImageContentBlock = Readonly<{
  type: "image";
  data: string;
  mimeType: ImageMimeType;
}>;
export type ResourceLinkContentBlock = Readonly<{
  type: "resource_link";
  name: string;
  uri: string;
}>;
export type PromptContentBlock = TextContentBlock | ImageContentBlock | ResourceLinkContentBlock;

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
  },
  /** Decodes optional initialization recovery plans from one product namespace. */
  initializeRecovery(meta: unknown, namespace: string): InitializeRecovery | undefined {
    const product = this.productMeta(meta, namespace);
    if (product?.sessionRecovery === undefined) return undefined;
    const recovery = this.decodeRecord(product.sessionRecovery, "ACP session recovery metadata");
    if (recovery.version !== 1 || !Array.isArray(recovery.sessions)) {
      throw new Error("ACP session recovery metadata has an unsupported version or sessions value");
    }
    const sessions = recovery.sessions.map((value, index) => {
      const plan = this.decodeRecord(value, `ACP recovery plan ${index}`);
      if (
        typeof plan.sessionId !== "string"
        || (plan.mode !== "watch" && plan.mode !== "resume")
        || (plan.runId !== undefined && typeof plan.runId !== "string")
        || (plan.nextSequence !== undefined && !this.validSequence(plan.nextSequence))
        || (plan.running !== undefined && typeof plan.running !== "boolean")
      ) {
        throw new Error(`ACP recovery plan ${index} is invalid`);
      }
      if (plan.mode === "watch" && (typeof plan.runId !== "string" || !this.validSequence(plan.nextSequence))) {
        throw new Error(`ACP watch recovery plan ${index} is missing its cursor`);
      }
      return {
        sessionId: plan.sessionId as SessionId,
        mode: plan.mode,
        ...(typeof plan.runId === "string" ? { runId: plan.runId as RunId } : {}),
        ...(this.validSequence(plan.nextSequence) ? { nextSequence: plan.nextSequence as EventSequence } : {}),
        ...(typeof plan.running === "boolean" ? { running: plan.running } : {})
      } satisfies SessionRecoveryPlan;
    });
    return { version: 1, sessions };
  },
  /** Decodes optional atomic projection metadata from one Session notification. */
  projectionGroupMeta(meta: unknown, namespace: string): ProjectionGroupMeta | undefined {
    const product = this.productMeta(meta, namespace);
    if (product?.sessionRecovery === undefined) return undefined;
    const group = this.decodeRecord(product.sessionRecovery, "ACP projection group metadata");
    if (
      typeof group.runId !== "string"
      || !Number.isSafeInteger(group.projectionIndex)
      || !Number.isSafeInteger(group.projectionCount)
      || (group.projectionIndex as number) < 0
      || (group.projectionCount as number) <= 0
      || (group.projectionIndex as number) >= (group.projectionCount as number)
      || (group.lastSequence !== undefined && !this.validSequence(group.lastSequence))
      || (group.operationPhase !== undefined && group.operationPhase !== "start" && group.operationPhase !== "end")
    ) {
      throw new Error("ACP projection group metadata is invalid");
    }
    return {
      runId: group.runId as RunId,
      projectionIndex: group.projectionIndex as number,
      projectionCount: group.projectionCount as number,
      ...(this.validSequence(group.lastSequence)
        ? { lastSequence: group.lastSequence as EventSequence }
        : {}),
      ...(group.operationPhase === "start" || group.operationPhase === "end"
        ? { operationPhase: group.operationPhase }
        : {})
    };
  },
  /** Returns whether a JSON value is a positive ACP event sequence. */
  validSequence(value: unknown): value is number {
    return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
  }
} as const;
