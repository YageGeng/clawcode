import type { PermissionPrompt, SessionSummary } from "../domain/model";

export type Brand<T, Name extends string> = T & { readonly __brand: Name };

export type SessionId = Brand<string, "SessionId">;
export type RunId = Brand<string, "RunId">;
export type EventSequence = Brand<number, "EventSequence">;
export type TurnId = Brand<string, "TurnId">;
export type MessageId = Brand<string, "MessageId">;
export type ToolCallId = Brand<string, "ToolCallId">;
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

export type SessionRecoveryCursor = Readonly<{
  sessionId: SessionId;
  runId: RunId;
  nextSequence: EventSequence;
}>;

export type ProjectionGroupMeta = Readonly<{
  runId: RunId;
  projectionIndex: number;
  projectionCount: number;
  lastSequence?: EventSequence;
  operationPhase?: "start" | "end";
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

export type SessionUpdateNotification = Readonly<{
  sessionId: SessionId;
  update: Readonly<Record<string, unknown>>;
  _meta?: AcpMeta;
}>;

export const AcpProtocol = {
  methods: {
    initialize: "initialize",
    sessionRequestPermission: "session/request_permission",
    sessionNew: "session/new",
    sessionList: "session/list",
    sessionResume: "session/resume",
    sessionPrompt: "session/prompt",
    sessionCancel: "session/cancel",
    sessionUpdate: "session/update"
  },
  /** `value` as a plain JSON object, or `undefined` for anything else (arrays and null included). */
  record(value: unknown): Record<string, unknown> | undefined {
    return typeof value === "object" && value !== null && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
  },
  /** `value` as a plain JSON object; throws naming `label` otherwise. */
  decodeRecord(value: unknown, label: string): Record<string, unknown> {
    const record = this.record(value);
    if (record === undefined) throw new Error(`${label} must be an object`);
    return record;
  },
  /** Returns only the configured product namespace from ACP `_meta`. */
  productMeta(meta: unknown, namespace: string): Record<string, unknown> | undefined {
    return this.record(this.record(meta)?.[namespace]);
  },
  /**
   * Expands one negotiated batch carrier into its ordered Session updates.
   *
   * The outer update comes first, without the batch key; nested items follow in
   * order. A malformed carrier throws so the caller resynchronizes the connection
   * instead of applying a partial frame.
   */
  expandSessionUpdateBatch(
    update: SessionUpdateNotification,
    namespace: string
  ): readonly SessionUpdateNotification[] {
    const product = this.productMeta(update._meta, namespace);
    if (product?.sessionUpdateBatch === undefined) return [update];
    const items = product.sessionUpdateBatch;
    if (!Array.isArray(items) || items.length === 0) {
      throw new Error("ACP session update batch is invalid");
    }
    const nested = items.map((item: unknown) => {
      const record = this.record(item);
      if (
        record === undefined
        || record.sessionId !== update.sessionId
        || this.productMeta(record._meta, namespace)?.sessionUpdateBatch !== undefined
      ) {
        throw new Error("ACP session update batch is invalid");
      }
      return record as SessionUpdateNotification;
    });
    // Keep every other product key on the outer update; only the carrier moves out.
    const outerProduct: Record<string, unknown> = { ...product };
    delete outerProduct.sessionUpdateBatch;
    const outer: SessionUpdateNotification = {
      ...update,
      _meta: { ...update._meta, [namespace]: outerProduct }
    };
    return [outer, ...nested];
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
  /** A listed Session as the app shows it: its title falls back to its id, its times come from product metadata. */
  sessionSummary(session: SessionInfo, namespace: string): SessionSummary {
    const details = this.productMeta(session._meta, namespace) ?? {};
    return {
      sessionId: session.sessionId,
      cwd: session.cwd,
      title: session.title?.trim() || session.sessionId,
      ...(typeof details.createdAtMs === "string" ? { createdAtMs: details.createdAtMs as TimestampMs } : {}),
      ...(typeof details.modifiedAtMs === "string" ? { modifiedAtMs: details.modifiedAtMs as TimestampMs } : {}),
    };
  },
  /** The prompt a `session/request_permission` asks, without its queue key; `undefined` when it names no Session, title or valid option. */
  permissionRequest(params: unknown): Omit<PermissionPrompt, "key"> | undefined {
    const request = this.record(params);
    const options = Array.isArray(request?.options)
      ? request.options.flatMap((option: unknown) => {
          const { optionId, name, kind } = this.record(option) ?? {};
          return typeof optionId === "string" && typeof name === "string" && typeof kind === "string" ? [{ optionId, name, kind }] : [];
        })
      : [];
    if (typeof request?.sessionId !== "string" || typeof request.title !== "string" || options.length === 0) return undefined;
    return {
      sessionId: request.sessionId as SessionId,
      title: request.title,
      ...(typeof request.description === "string" ? { description: request.description } : {}),
      options,
    };
  },
  /** Returns whether a JSON value is a positive ACP event sequence. */
  validSequence(value: unknown): value is number {
    return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
  }
} as const;
