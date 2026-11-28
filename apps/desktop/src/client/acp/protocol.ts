import type { AuthMethod, PermissionPrompt, ProviderSignIn, SessionOwner, SessionSummary, SignInState } from "../domain/model";

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

export type NewSessionResult = Readonly<{ sessionId: SessionId; configOptions?: unknown; availableCommands?: unknown }>;

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
    sessionSetConfigOption: "session/set_config_option",
    sessionSetTitle: "session/set_title",
    sessionDelete: "session/delete",
    authLogin: "auth/login",
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
      ...(typeof details.parentSessionId === "string" ? { parentSessionId: details.parentSessionId as SessionId } : {}),
      ...this.sessionOwner(details.ownership),
    };
  },
  /**
   * The named sign-in methods of an `initialize` reply; malformed entries are left out. ACP v2 names a method
   * `methodId` (`{ type: "agent", methodId, name }`); v1 called it `id`.
   */
  authMethods(reply: unknown): readonly AuthMethod[] {
    const methods = this.record(reply)?.authMethods;
    if (!Array.isArray(methods)) return [];
    return methods.flatMap((value: unknown) => {
      const { methodId, name, description } = this.record(value) ?? {};
      const id = methodId ?? this.record(value)?.id;
      return typeof id === "string" && typeof name === "string"
        ? [{ id, name, ...(typeof description === "string" ? { description } : {}) }] : [];
    });
  },
  /** A `_clawcode/auth/status` reply; an unknown state reads as undecided. Throws for a reply that is not a list. */
  signInStatuses(reply: unknown): readonly ProviderSignIn[] {
    if (!Array.isArray(reply)) throw new Error("Invalid sign-in status reply");
    return reply.flatMap((value: unknown) => {
      const { methodId, status } = this.record(value) ?? {};
      if (typeof methodId !== "string") return [];
      const state: SignInState = status === "signedIn" || status === "signInRequired" ? status : "unknown";
      return [{ methodId, status: state }];
    });
  },
  /**
   * The entry of a `_clawcode/session/tree` reply that recorded message `messageId` as `role` ("user" or "assistant"),
   * with its parent (`null` at the root); `undefined` when the tree holds no such message.
   */
  treeEntry(reply: unknown, messageId: MessageId, role: "user" | "assistant"): { entryId: string; parentId: string | null } | undefined {
    const entries = this.record(reply)?.entries;
    if (!Array.isArray(entries)) throw new Error("Invalid session tree reply");
    for (const value of entries) {
      const entry = this.record(value);
      const payload = this.record(entry?.payload);
      if (entry?.kind !== "message" || payload?.message_id !== messageId || this.record(payload.content)?.type !== role) continue;
      if (typeof entry.entryId !== "string") return undefined;
      return { entryId: entry.entryId, parentId: typeof entry.parentId === "string" ? entry.parentId : null };
    }
    return undefined;
  },
  /** The `owner` field of a summary; older hosts and incomplete edges contribute nothing. */
  sessionOwner(value: unknown): { owner?: SessionOwner } {
    const { sessionId, name, nickname } = this.record(value) ?? {};
    return typeof sessionId === "string" && typeof name === "string" && typeof nickname === "string"
      ? { owner: { sessionId: sessionId as SessionId, name, nickname } }
      : {};
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
    const subject = this.record(request.subject);
    const tool = subject?.type === "tool_call" ? this.record(subject.toolCall) : undefined;
    const plan = tool?.kind === "switch_mode" && Array.isArray(tool.content)
      ? tool.content.flatMap((item: unknown) => {
          const block = this.record(this.record(item)?.content);
          return block?.type === "text" && typeof block.text === "string" ? [block.text] : [];
        }).join("\n") : undefined;
    return {
      sessionId: request.sessionId as SessionId,
      title: request.title,
      ...(typeof request.description === "string" ? { description: request.description } : {}),
      ...(plan ? { plan } : {}),
      options,
    };
  },
  /** Returns whether a JSON value is a positive ACP event sequence. */
  validSequence(value: unknown): value is number {
    return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
  }
} as const;
