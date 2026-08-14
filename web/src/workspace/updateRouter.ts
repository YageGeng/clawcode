import { AcpProtocol } from "../acp/protocol";
import type { MessageId, SessionId, SessionUpdateNotification } from "../acp/protocol";
import type {
  AssistantDiagnostics,
  CompactionReason,
  EventOrder,
  MessageEntity,
  ModelUsage,
  SessionEvent,
  ToolCallEntity
} from "../domain/model";
import { useWorkspaceStore } from "./store";
import type { WorkspaceAction } from "./state";

/** Decodes ACP session updates and routes them into the active UI projection. */
export class SessionUpdateRouter {
  readonly namespace: string;
  private readonly dispatch: (action: WorkspaceAction) => void;
  private readonly refreshSessionRuntime: (sessionId: SessionId) => Promise<void>;
  private receivedOrder = 0;

  /** Creates a router bound to one product namespace and workspace projection. */
  constructor(
    namespace: string,
    dispatch: (action: WorkspaceAction) => void,
    refreshSessionRuntime: (sessionId: SessionId) => Promise<void>
  ) {
    this.namespace = namespace;
    this.dispatch = dispatch;
    this.refreshSessionRuntime = refreshSessionRuntime;
  }

  /** Applies one update without allowing background sessions to mutate the active transcript. */
  apply(notification: SessionUpdateNotification): void {
    const update = notification.update;
    const kind = update.sessionUpdate;
    if (kind === "session_info_update" && typeof update.title === "string") {
      this.dispatch({ type: "session/title", sessionId: notification.sessionId, title: update.title });
      return;
    }
    if (useWorkspaceStore.getState().activeSessionId !== notification.sessionId) return;

    const updateContent = typeof update.content === "object" && update.content !== null && !Array.isArray(update.content) ? update.content as Record<string, unknown> : undefined;
    const rawMeta = update._meta ?? notification._meta;
    const meta = AcpProtocol.eventMeta(rawMeta, this.namespace) ?? AcpProtocol.eventMeta(updateContent?._meta, this.namespace);
    const productMeta = AcpProtocol.productMeta(rawMeta, this.namespace) ?? AcpProtocol.productMeta(updateContent?._meta, this.namespace);
    const order: EventOrder = { receivedOrder: ++this.receivedOrder };
    if (typeof kind === "string") {
      const event: SessionEvent = { type: kind, payload: update, order, ...(meta === undefined ? {} : { meta }) };
      this.dispatch({ type: "event/received", event });
    }
    if ((kind === "agent_message_chunk" || kind === "user_message_chunk") && typeof update.messageId === "string" && typeof update.content === "object" && update.content !== null) {
      const content = update.content as Record<string, unknown>;
      const contentMeta = AcpProtocol.eventMeta(content._meta, this.namespace) ?? meta;
      if (typeof content.text === "string" && contentMeta !== undefined) this.dispatch({ type: "message/text-delta", messageId: update.messageId as MessageId, role: kind === "user_message_chunk" ? "user" : "assistant", delta: content.text, meta: contentMeta, order });
      return;
    }
    if (kind === "agent_thought_chunk" && typeof update.messageId === "string" && typeof update.content === "object" && update.content !== null) {
      const content = update.content as Record<string, unknown>;
      const contentMeta = AcpProtocol.eventMeta(content._meta, this.namespace) ?? meta;
      if (typeof content.text === "string" && contentMeta !== undefined) this.dispatch({ type: "message/reasoning-delta", messageId: update.messageId as MessageId, delta: content.text, meta: contentMeta, order });
      return;
    }
    if ((kind === "agent_message" || kind === "user_message") && typeof update.messageId === "string" && meta !== undefined) {
      const existing = useWorkspaceStore.getState().messages.get(update.messageId as MessageId);
      // ACP v2 whole-message updates preserve omitted content and replace concrete content, including explicit clears.
      const text = !("content" in update) ? existing?.text ?? "" : Array.isArray(update.content)
        ? update.content.filter((block): block is Record<string, unknown> => typeof block === "object" && block !== null && !Array.isArray(block)).map((block) => typeof block.text === "string" ? block.text : "").join("")
        : "";
      const assistant = kind === "agent_message" ? this.decodeAssistantDiagnostics(productMeta?.assistant) : undefined;
      const message: MessageEntity = { messageId: update.messageId as MessageId, turnId: meta.turnId, role: kind === "user_message" ? "user" : "assistant", text, reasoning: existing?.reasoning ?? "", timestampMs: meta.timestampMs, startedAtMs: existing?.startedAtMs ?? meta.timestampMs, endedAtMs: meta.timestampMs, streaming: false, ...(assistant === undefined ? (existing?.assistant === undefined ? {} : { assistant: existing.assistant }) : { assistant }) };
      this.dispatch({ type: "message/upserted", message, order });
      return;
    }
    if (kind === "agent_thought" && typeof update.messageId === "string" && meta !== undefined) {
      const messageId = update.messageId as MessageId;
      const previous = useWorkspaceStore.getState().messages.get(messageId);
      // Thought upserts follow the same omitted/clear/replace contract as visible messages.
      const reasoning = !("content" in update) ? previous?.reasoning ?? "" : Array.isArray(update.content)
        ? update.content.filter((block): block is Record<string, unknown> => typeof block === "object" && block !== null && !Array.isArray(block)).map((block) => typeof block.text === "string" ? block.text : "").join("")
        : "";
      const message: MessageEntity = previous === undefined ? { messageId, turnId: meta.turnId, role: "assistant", text: "", reasoning, timestampMs: meta.timestampMs, startedAtMs: meta.timestampMs, endedAtMs: meta.timestampMs, streaming: false } : { ...previous, reasoning, endedAtMs: meta.timestampMs };
      this.dispatch({ type: "message/upserted", message, order });
      return;
    }
    if (kind === "tool_call_update" && typeof update.toolCallId === "string") {
      const toolCallId = update.toolCallId as ToolCallEntity["toolCallId"];
      const previous = useWorkspaceStore.getState().tools.get(toolCallId);
      const status = update.status === "failed" || update.status === "completed" || update.status === "in_progress" ? update.status : previous?.status ?? "pending";
      const tool: ToolCallEntity = {
        toolCallId,
        title: typeof update.title === "string" ? update.title : previous?.title ?? update.toolCallId,
        status,
        ...(update.rawInput === undefined ? (previous?.rawInput === undefined ? {} : { rawInput: previous.rawInput }) : { rawInput: update.rawInput }),
        ...(update.rawOutput === undefined ? (previous?.rawOutput === undefined ? {} : { rawOutput: previous.rawOutput }) : { rawOutput: update.rawOutput }),
        // ACP v2 tool content follows the same omitted/clear/replace upsert contract as messages.
        content: !("content" in update) ? previous?.content ?? [] : Array.isArray(update.content) ? update.content : [],
        ...(previous?.startedAtMs === undefined && meta !== undefined ? { startedAtMs: meta.timestampMs } : previous?.startedAtMs === undefined ? {} : { startedAtMs: previous.startedAtMs }),
        ...(status === "completed" || status === "failed" ? (meta === undefined ? {} : { endedAtMs: meta.timestampMs }) : {}),
        ...(meta === undefined ? (previous?.meta === undefined ? {} : { meta: previous.meta }) : { meta })
      };
      this.dispatch({ type: "tool/upserted", tool, order });
      return;
    }
    if (kind === "usage_update" && typeof update.used === "number" && typeof update.size === "number" && meta !== undefined) {
      const exact = this.decodeModelUsage(productMeta?.usage);
      if (exact !== undefined && update.used > 0) this.dispatch({ type: "usage/changed", usage: { used: update.used, size: update.size, exact, updatedAtMs: meta.timestampMs } });
      return;
    }
    if (kind === "state_update" && typeof update.state === "string") {
      this.dispatch({ type: "running/changed", running: update.state === "running" });
      if (update.state === "idle") {
        void this.refreshSessionRuntime(notification.sessionId).catch((reason: unknown) => this.dispatch({ type: "diagnostic/added", message: reason instanceof Error ? reason.message : String(reason) }));
      }
      return;
    }
    if (kind === `_${this.namespace}/event`) {
      const payload = typeof update.payload === "object" && update.payload !== null && !Array.isArray(update.payload) ? update.payload as Record<string, unknown> : undefined;
      if (payload === undefined) return;
      const event = payload.event;
      if (event === "retry_scheduled" && meta !== undefined && typeof payload.attempt === "number" && typeof payload.max_attempts === "number" && typeof payload.delay_ms === "number" && typeof payload.error === "string") {
        this.dispatch({ type: "retry/changed", retry: { type: "waiting", attempt: payload.attempt, maxAttempts: payload.max_attempts, scheduledAtMs: meta.timestampMs, delayMs: payload.delay_ms, error: payload.error } });
      } else if (event === "retry_start" && typeof payload.attempt === "number" && typeof payload.max_attempts === "number") {
        this.dispatch({ type: "retry/changed", retry: { type: "running", attempt: payload.attempt, maxAttempts: payload.max_attempts } });
      } else if (event === "retry_end" && typeof payload.attempt === "number" && typeof payload.success === "boolean") {
        this.dispatch({ type: "retry/changed", retry: { type: "finished", attempt: payload.attempt, success: payload.success, ...(typeof payload.final_error === "string" ? { finalError: payload.final_error } : {}) } });
      } else if (event === "compaction_start" && meta !== undefined) {
        const reason = this.decodeCompactionReason(payload.reason);
        if (reason !== undefined) this.dispatch({ type: "compaction/changed", compaction: { type: "running", reason, startedAtMs: meta.timestampMs } });
      } else if (event === "compaction_end" && meta !== undefined) {
        const reason = this.decodeCompactionReason(payload.reason);
        if (reason !== undefined) {
          this.dispatch({ type: "compaction/changed", compaction: { type: "finished", reason, endedAtMs: meta.timestampMs } });
          this.dispatch({ type: "usage/changed", usage: undefined });
        }
      }
      return;
    }
    if (typeof kind === "string" && kind.startsWith("_")) return;
    this.dispatch({ type: "diagnostic/added", message: `Unknown session update: ${String(kind)}` });
  }

  /** Decodes exact string token fields carried in product metadata. */
  private decodeModelUsage(value: unknown): ModelUsage | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    const usage = value as Record<string, unknown>;
    if (typeof usage.input_tokens !== "string" || typeof usage.output_tokens !== "string" || typeof usage.cache_read_tokens !== "string" || typeof usage.cache_write_tokens !== "string" || typeof usage.total_tokens !== "string" || (usage.reasoning_tokens !== undefined && typeof usage.reasoning_tokens !== "string")) return undefined;
    return { inputTokens: usage.input_tokens, outputTokens: usage.output_tokens, cacheReadTokens: usage.cache_read_tokens, cacheWriteTokens: usage.cache_write_tokens, ...(typeof usage.reasoning_tokens === "string" ? { reasoningTokens: usage.reasoning_tokens } : {}), totalTokens: usage.total_tokens };
  }

  /** Decodes provider diagnostics attached to a complete Assistant update. */
  private decodeAssistantDiagnostics(value: unknown): AssistantDiagnostics | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    const assistant = value as Record<string, unknown>;
    const usage = this.decodeModelUsage(assistant.usage);
    if (typeof assistant.provider_id !== "string" || typeof assistant.model_id !== "string" || typeof assistant.stop_reason !== "string" || usage === undefined) return undefined;
    return { providerId: assistant.provider_id, modelId: assistant.model_id, stopReason: assistant.stop_reason, usage, ...(typeof assistant.raw_stop_reason === "string" ? { rawStopReason: assistant.raw_stop_reason } : {}), ...(typeof assistant.error === "string" ? { error: assistant.error } : {}) };
  }

  /** Narrows the three persisted pi-compatible compaction reasons. */
  private decodeCompactionReason(value: unknown): CompactionReason | undefined {
    return value === "manual" || value === "threshold" || value === "overflow" ? value : undefined;
  }
}
