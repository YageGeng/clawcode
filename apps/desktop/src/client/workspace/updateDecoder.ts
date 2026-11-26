import { AcpProtocol } from "../acp/protocol";
import type { EventMeta, MessageId, SessionId, SessionUpdateNotification, TimestampMs } from "../acp/protocol";
import type { EventOrder, MessageEntity, SessionNotice, ToolCallEntity, ToolKind } from "../domain/model";
import { fileChanges } from "../turns";
import type { SessionEntities, SessionWorkspaceAction } from "./sessionState";
import type { WorkspaceAction } from "./state";

export type DecodedSessionUpdate =
  | Readonly<{ scope: "global"; actions: readonly WorkspaceAction[]; refreshRuntime: false }>
  | Readonly<{ scope: "session"; actions: readonly SessionWorkspaceAction[]; refreshRuntime: boolean }>;

/** What every Session update carries besides its own fields. */
type UpdateContext = Readonly<{
  update: Readonly<Record<string, unknown>>;
  meta: EventMeta | undefined;
  productMeta: Record<string, unknown> | undefined;
  order: EventOrder;
  entities: SessionEntities;
}>;

const TOOL_KINDS: ReadonlySet<ToolKind> = new Set<ToolKind>(["read", "edit", "delete", "move", "search", "execute", "think", "fetch", "switch_mode", "other"]);

/** Joins the text of ACP content blocks; a value that is not a block array has no text. */
function contentText(content: unknown): string {
  if (!Array.isArray(content)) return "";
  return content.map((block: unknown) => {
    const text = AcpProtocol.record(block)?.text;
    return typeof text === "string" ? text : "";
  }).join("");
}

/**
 * Narrows ACP wire updates into typed workspace actions without owning UI state. Only what the
 * app shows is decoded: titles, notices, messages, tool calls and the run state; other updates
 * are ignored.
 */
export class SessionUpdateDecoder {
  readonly namespace: string;

  /** Creates a decoder for one product extension namespace. */
  constructor(namespace: string) {
    this.namespace = namespace;
  }

  /** Decodes one ACP notification while preserving omitted-field upsert semantics. */
  decode(notification: SessionUpdateNotification, entities: SessionEntities, receivedOrder: number): DecodedSessionUpdate {
    const update = notification.update;
    const kind = update.sessionUpdate;
    if (kind === "session_info_update" && (typeof update.title === "string" || update.title === null)) {
      // A null title clears the name; lists show an untitled Session by its id.
      const title = update.title?.trim() || notification.sessionId;
      return { scope: "global", actions: [{ type: "session/title", sessionId: notification.sessionId, title }], refreshRuntime: false };
    }
    if (kind === "notice" && typeof update.title === "string") {
      const notice: SessionNotice = {
        id: `${notification.sessionId}:${receivedOrder}`,
        sessionId: notification.sessionId,
        title: update.title,
        ...(typeof update.description === "string" ? { description: update.description } : {})
      };
      return { scope: "global", actions: [{ type: "notice/added", notice }], refreshRuntime: false };
    }
    if (kind === "state_update" && (update.state === "running" || update.state === "requires_action" || update.state === "idle")) {
      // Waiting on the user is still foreground work; unknown states leave the flag unchanged.
      return { scope: "session", actions: [{ type: "running/changed", running: update.state !== "idle" }], refreshRuntime: update.state === "idle" };
    }

    const content = AcpProtocol.record(update.content);
    const rawMeta = update._meta ?? notification._meta;
    const meta = AcpProtocol.eventMeta(rawMeta, this.namespace) ?? AcpProtocol.eventMeta(content?._meta, this.namespace);
    // Kernel sequence is authoritative; arrival order only breaks ties between
    // multiple ACP projections produced from the same domain event.
    const context: UpdateContext = {
      update,
      meta,
      productMeta: AcpProtocol.productMeta(rawMeta, this.namespace) ?? AcpProtocol.productMeta(content?._meta, this.namespace),
      order: { sequence: meta?.sequence ?? receivedOrder, receivedOrder },
      entities
    };
    const action = this.sessionAction(kind, context);
    return { scope: "session", actions: action === undefined ? [] : [action], refreshRuntime: false };
  }

  /** The Session action of one message or tool update, if it is one the app shows. */
  private sessionAction(kind: unknown, context: UpdateContext): SessionWorkspaceAction | undefined {
    const { update, meta, productMeta, order } = context;
    switch (kind) {
      case "agent_message_chunk":
      case "user_message_chunk":
      case "agent_thought_chunk": {
        const content = AcpProtocol.record(update.content);
        const contentMeta = AcpProtocol.eventMeta(content?._meta, this.namespace) ?? meta;
        if (typeof update.messageId !== "string" || typeof content?.text !== "string" || contentMeta === undefined) return undefined;
        const messageId = update.messageId as MessageId;
        return kind === "agent_thought_chunk"
          ? { type: "message/reasoning-delta", messageId, delta: content.text, meta: contentMeta, order }
          : { type: "message/text-delta", messageId, role: kind === "user_message_chunk" ? "user" : "assistant", delta: content.text, meta: contentMeta, order };
      }
      case "user_message":
        // A `!bash` prompt is shown by its tool, and a replayed branch summary is not user input.
        if (productMeta?.origin === "user_bash" || productMeta?.origin === "branch_summary") return undefined;
        return this.message(context, "user");
      case "agent_message":
        return this.message(context, "assistant");
      case "agent_thought":
        return this.thought(context);
      case "tool_call_update":
        return this.tool(context);
      default:
        return undefined;
    }
  }

  /** A whole user or agent message; ACP v2 keeps omitted content and replaces concrete content, clears included. */
  private message(context: UpdateContext, role: MessageEntity["role"]): SessionWorkspaceAction | undefined {
    const { update, meta } = context;
    if (typeof update.messageId !== "string" || meta === undefined) return undefined;
    const messageId = update.messageId as MessageId;
    const existing = context.entities.messages.get(messageId);
    const timing = this.messageTiming(context.productMeta?.messageTiming);
    const message: MessageEntity = {
      messageId,
      role,
      text: "content" in update ? contentText(update.content) : existing?.text ?? "",
      reasoning: existing?.reasoning ?? "",
      startedAtMs: timing?.startedAtMs ?? existing?.startedAtMs ?? meta.timestampMs,
      endedAtMs: timing?.endedAtMs ?? meta.timestampMs
    };
    return { type: "message/upserted", message, order: context.order };
  }

  /** A whole agent thought, merged into its message with the same omitted / clear / replace rules. */
  private thought(context: UpdateContext): SessionWorkspaceAction | undefined {
    const { update, meta } = context;
    if (typeof update.messageId !== "string" || meta === undefined) return undefined;
    const messageId = update.messageId as MessageId;
    const previous = context.entities.messages.get(messageId);
    const timing = this.messageTiming(context.productMeta?.messageTiming);
    const reasoning = "content" in update ? contentText(update.content) : previous?.reasoning ?? "";
    const message: MessageEntity = previous === undefined
      ? { messageId, role: "assistant", text: "", reasoning, startedAtMs: timing?.startedAtMs ?? meta.timestampMs, endedAtMs: timing?.endedAtMs ?? meta.timestampMs }
      : { ...previous, reasoning, ...(timing === undefined ? {} : timing) };
    return { type: "message/upserted", message, order: context.order };
  }

  /** One tool call, merged over its previous version: omitted fields keep their values. */
  private tool(context: UpdateContext): SessionWorkspaceAction | undefined {
    const { update, meta } = context;
    if (typeof update.toolCallId !== "string") return undefined;
    const toolCallId = update.toolCallId as ToolCallEntity["toolCallId"];
    const previous = context.entities.tools.get(toolCallId);
    const status = update.status === "failed" || update.status === "completed" || update.status === "in_progress" || update.status === "cancelled"
      ? update.status
      : previous?.status ?? "pending";
    const kind = typeof update.kind === "string" && TOOL_KINDS.has(update.kind as ToolKind) ? update.kind as ToolKind : previous?.kind;
    const subagentSessionId = this.subagentSessionId(update.rawOutput) ?? previous?.subagentSessionId;
    const ended = status === "completed" || status === "failed" || status === "cancelled";
    const tool: ToolCallEntity = {
      toolCallId,
      title: typeof update.title === "string" ? update.title : previous?.title ?? update.toolCallId,
      status,
      ...(kind === undefined ? {} : { kind }),
      // A discovered child stays even when a later cancellation carries no details.
      ...(subagentSessionId === undefined ? {} : { subagentSessionId }),
      // ACP v2 content follows the omitted / clear / replace rules; only its diffs are kept, as counts.
      changes: !("content" in update) ? previous?.changes ?? [] : fileChanges(Array.isArray(update.content) ? update.content : []),
      ...(ended && meta !== undefined ? { endedAtMs: meta.timestampMs } : {})
    };
    return { type: "tool/upserted", tool, order: context.order };
  }

  /** The child Session a spawn tool's result names: live results and persisted replay wrap their details differently. */
  private subagentSessionId(rawOutput: unknown): SessionId | undefined {
    const output = AcpProtocol.record(rawOutput);
    const details = AcpProtocol.record(output?.details) ?? AcpProtocol.record(AcpProtocol.record(output?.content)?.details);
    return details?.type === "subagent" && typeof details.session_id === "string" ? details.session_id as SessionId : undefined;
  }

  /** Restores precision-safe persisted message timing during ACP replay. */
  private messageTiming(value: unknown): Readonly<{ startedAtMs: TimestampMs; endedAtMs: TimestampMs }> | undefined {
    const timing = AcpProtocol.record(value);
    if (typeof timing?.started_at_ms !== "string" || typeof timing.ended_at_ms !== "string") return undefined;
    return { startedAtMs: timing.started_at_ms as TimestampMs, endedAtMs: timing.ended_at_ms as TimestampMs };
  }
}
