import { AcpProtocol } from "../acp/protocol";
import type { EntryId, MessageId, SessionUpdateNotification, TimestampMs, TurnId } from "../acp/protocol";
import type {
  AvailableCommandEntity,
  AssistantDiagnostics,
  CompactionEntity,
  CompactionReason,
  EventOrder,
  McpElicitation,
  MessageEntity,
  ModelUsage,
  SessionEvent,
  SlashCommandAliasKind,
  SlashCommandMessageMeta,
  SlashCommandSource,
  ToolCallEntity
} from "../domain/model";
import type { WorkspaceAction, WorkspaceState } from "./state";

export type DecodedSessionUpdate = Readonly<{
  scope: "global" | "session";
  actions: readonly WorkspaceAction[];
  refreshRuntime: boolean;
}>;

/** Narrows ACP wire updates into typed workspace actions without owning UI state. */
export class SessionUpdateDecoder {
  readonly namespace: string;

  /** Creates a decoder for one product extension namespace. */
  constructor(namespace: string) {
    this.namespace = namespace;
  }

  /** Decodes one ACP notification while preserving omitted-field upsert semantics. */
  decode(
    notification: SessionUpdateNotification,
    state: WorkspaceState,
    receivedOrder: number
  ): DecodedSessionUpdate {
    const update = notification.update;
    const kind = update.sessionUpdate;
    if (kind === "session_info_update" && typeof update.title === "string") {
      return {
        scope: "global",
        actions: [{ type: "session/title", sessionId: notification.sessionId, title: update.title }],
        refreshRuntime: false
      };
    }

    const actions: WorkspaceAction[] = [];
    const updateContent = typeof update.content === "object" && update.content !== null && !Array.isArray(update.content) ? update.content as Record<string, unknown> : undefined;
    const rawMeta = update._meta ?? notification._meta;
    const meta = AcpProtocol.eventMeta(rawMeta, this.namespace) ?? AcpProtocol.eventMeta(updateContent?._meta, this.namespace);
    const productMeta = AcpProtocol.productMeta(rawMeta, this.namespace) ?? AcpProtocol.productMeta(updateContent?._meta, this.namespace);
    // Kernel sequence is authoritative; arrival order only breaks ties between
    // multiple ACP projections produced from the same domain event.
    const order: EventOrder = {
      sequence: meta?.sequence ?? receivedOrder,
      receivedOrder
    };
    if (typeof kind === "string") {
      const event: SessionEvent = { type: kind, payload: update, order, ...(meta === undefined ? {} : { meta }) };
      actions.push({ type: "event/received", event });
    }

    if (kind === "available_commands_update") {
      const decoded = this.decodeAvailableCommands(update.availableCommands);
      if (decoded.commands !== undefined) {
        actions.push({ type: "commands/replaced", commands: decoded.commands });
      }
      actions.push(...decoded.diagnostics.map((message): WorkspaceAction => ({ type: "diagnostic/added", message })));
    } else if ((kind === "agent_message_chunk" || kind === "user_message_chunk") && typeof update.messageId === "string" && typeof update.content === "object" && update.content !== null) {
      const content = update.content as Record<string, unknown>;
      const contentMeta = AcpProtocol.eventMeta(content._meta, this.namespace) ?? meta;
      if (typeof content.text === "string" && contentMeta !== undefined) {
        actions.push({ type: "message/text-delta", messageId: update.messageId as MessageId, role: kind === "user_message_chunk" ? "user" : "assistant", delta: content.text, meta: contentMeta, order });
      }
    } else if (kind === "agent_thought_chunk" && typeof update.messageId === "string" && typeof update.content === "object" && update.content !== null) {
      const content = update.content as Record<string, unknown>;
      const contentMeta = AcpProtocol.eventMeta(content._meta, this.namespace) ?? meta;
      if (typeof content.text === "string" && contentMeta !== undefined) {
        actions.push({ type: "message/reasoning-delta", messageId: update.messageId as MessageId, delta: content.text, meta: contentMeta, order });
      }
    } else if ((kind === "agent_message" || kind === "user_message") && typeof update.messageId === "string" && meta !== undefined) {
      const messageId = update.messageId as MessageId;
      const existing = state.messages.get(messageId);
      // ACP v2 whole-message updates preserve omitted content and replace
      // concrete content, including explicit clears.
      const text = !("content" in update) ? existing?.text ?? "" : Array.isArray(update.content)
        ? update.content.filter((block): block is Record<string, unknown> => typeof block === "object" && block !== null && !Array.isArray(block)).map((block) => typeof block.text === "string" ? block.text : "").join("")
        : "";
      const assistant = kind === "agent_message" ? this.decodeAssistantDiagnostics(productMeta?.assistant) : undefined;
      const slashCommand = this.decodeSlashCommandMetadata(productMeta?.slashCommand);
      const timing = this.decodeMessageTiming(productMeta?.messageTiming);
      const message: MessageEntity = { messageId, turnId: meta.turnId, role: kind === "user_message" ? "user" : "assistant", text, reasoning: existing?.reasoning ?? "", timestampMs: timing?.timestampMs ?? meta.timestampMs, startedAtMs: timing?.startedAtMs ?? existing?.startedAtMs ?? meta.timestampMs, endedAtMs: timing?.endedAtMs ?? meta.timestampMs, streaming: false, ...(assistant === undefined ? (existing?.assistant === undefined ? {} : { assistant: existing.assistant }) : { assistant }), ...(slashCommand === undefined ? (existing?.slashCommand === undefined ? {} : { slashCommand: existing.slashCommand }) : { slashCommand }) };
      actions.push({ type: "message/upserted", message, order });
    } else if (kind === "agent_thought" && typeof update.messageId === "string" && meta !== undefined) {
      const messageId = update.messageId as MessageId;
      const previous = state.messages.get(messageId);
      const timing = this.decodeMessageTiming(productMeta?.messageTiming);
      // Thought upserts follow the same omitted/clear/replace contract as visible messages.
      const reasoning = !("content" in update) ? previous?.reasoning ?? "" : Array.isArray(update.content)
        ? update.content.filter((block): block is Record<string, unknown> => typeof block === "object" && block !== null && !Array.isArray(block)).map((block) => typeof block.text === "string" ? block.text : "").join("")
        : "";
      const message: MessageEntity = previous === undefined ? { messageId, turnId: meta.turnId, role: "assistant", text: "", reasoning, timestampMs: timing?.timestampMs ?? meta.timestampMs, startedAtMs: timing?.startedAtMs ?? meta.timestampMs, endedAtMs: timing?.endedAtMs ?? meta.timestampMs, streaming: false } : { ...previous, reasoning, ...(timing === undefined ? {} : timing) };
      actions.push({ type: "message/upserted", message, order });
    } else if (kind === "tool_call_update" && typeof update.toolCallId === "string") {
      const toolCallId = update.toolCallId as ToolCallEntity["toolCallId"];
      const previous = state.tools.get(toolCallId);
      const rawOutput = update.rawOutput ?? previous?.rawOutput;
      const outputRecord = typeof rawOutput === "object" && rawOutput !== null && !Array.isArray(rawOutput) ? rawOutput as Record<string, unknown> : undefined;
      const messageContent = typeof outputRecord?.content === "object" && outputRecord.content !== null && !Array.isArray(outputRecord.content) ? outputRecord.content as Record<string, unknown> : undefined;
      const details = typeof outputRecord?.details === "object" && outputRecord.details !== null && !Array.isArray(outputRecord.details)
        ? outputRecord.details as Record<string, unknown>
        : typeof messageContent?.details === "object" && messageContent.details !== null && !Array.isArray(messageContent.details)
          ? messageContent.details as Record<string, unknown>
          : undefined;
      // ACP v2 has no native blocked status, so the standard failed status is
      // refined only when the typed product result explicitly carries it.
      const status = update.status === "failed" && details?.type === "blocked"
        ? "blocked"
        : update.status === "failed" || update.status === "completed" || update.status === "in_progress"
          ? update.status
          : previous?.status ?? "pending";
      const tool: ToolCallEntity = {
        toolCallId,
        title: typeof update.title === "string" ? update.title : previous?.title ?? update.toolCallId,
        status,
        ...(update.rawInput === undefined ? (previous?.rawInput === undefined ? {} : { rawInput: previous.rawInput }) : { rawInput: update.rawInput }),
        ...(rawOutput === undefined ? {} : { rawOutput }),
        // ACP v2 tool content follows the same omitted/clear/replace upsert contract as messages.
        content: !("content" in update) ? previous?.content ?? [] : Array.isArray(update.content) ? update.content : [],
        ...(previous?.startedAtMs === undefined && meta !== undefined ? { startedAtMs: meta.timestampMs } : previous?.startedAtMs === undefined ? {} : { startedAtMs: previous.startedAtMs }),
        ...(status === "completed" || status === "failed" || status === "blocked" ? (meta === undefined ? {} : { endedAtMs: meta.timestampMs }) : {}),
        ...(meta === undefined ? (previous?.meta === undefined ? {} : { meta: previous.meta }) : { meta })
      };
      actions.push({ type: "tool/upserted", tool, order });
    } else if (kind === "usage_update" && typeof update.used === "number" && typeof update.size === "number" && meta !== undefined) {
      const exact = this.decodeModelUsage(productMeta?.usage);
      if (exact !== undefined && update.used > 0) {
        actions.push({ type: "usage/changed", usage: { used: update.used, size: update.size, exact, updatedAtMs: meta.timestampMs } });
      }
    } else if (kind === "state_update" && typeof update.state === "string") {
      actions.push({ type: "running/changed", running: update.state === "running" });
      return { scope: "session", actions, refreshRuntime: update.state === "idle" };
    } else if (kind === `_${this.namespace}/event`) {
      const payload = typeof update.payload === "object" && update.payload !== null && !Array.isArray(update.payload) ? update.payload as Record<string, unknown> : undefined;
      if (payload !== undefined) {
        const event = payload.event;
        if (event === "message_end" && meta !== undefined && typeof payload.message === "object" && payload.message !== null && !Array.isArray(payload.message)) {
          const message = payload.message as Record<string, unknown>;
          const identity = typeof message.identity === "object" && message.identity !== null && !Array.isArray(message.identity) ? message.identity as Record<string, unknown> : undefined;
          // AgentMessage flattens identity on the wire; retain nested support for
          // previously persisted extension payloads written before that contract.
          const messageId = typeof message.message_id === "string" ? message.message_id : typeof identity?.message_id === "string" ? identity.message_id : undefined;
          const content = typeof message.content === "object" && message.content !== null && !Array.isArray(message.content) ? message.content as Record<string, unknown> : undefined;
          const extension = content?.type === "extension" && typeof content.extension === "object" && content.extension !== null && !Array.isArray(content.extension) ? content.extension as Record<string, unknown> : undefined;
          if (messageId !== undefined && extension !== undefined && extension.display !== false && typeof extension.extension_id === "string" && typeof extension.custom_type === "string") {
            const details = typeof extension.details === "object" && extension.details !== null && !Array.isArray(extension.details) ? extension.details as Record<string, unknown> : undefined;
            actions.push({ type: "extension/upserted", extension: { id: messageId, turnId: meta.turnId, extensionId: extension.extension_id, customType: extension.custom_type, blocks: Array.isArray(extension.blocks) ? extension.blocks : [], ...(extension.details === undefined ? {} : { details: extension.details }), timestampMs: meta.timestampMs, kind: details?.kind === "handler_error" ? "error" : "message" }, order });
          } else if (messageId !== undefined && content?.type === "bash_execution" && typeof content.bash === "object" && content.bash !== null && !Array.isArray(content.bash)) {
            const bash = content.bash as Record<string, unknown>;
            const result = typeof bash.result === "object" && bash.result !== null && !Array.isArray(bash.result) ? bash.result as Record<string, unknown> : undefined;
            const disposition = typeof result?.disposition === "object" && result.disposition !== null && !Array.isArray(result.disposition) ? result.disposition as Record<string, unknown> : undefined;
            const timing = this.decodeMessageTiming(productMeta?.messageTiming);
            const typedDisposition = disposition?.type === "completed"
              ? { type: "completed" as const }
              : disposition?.type === "blocked" && typeof disposition.reason === "string"
                ? { type: "blocked" as const, reason: disposition.reason }
                : undefined;
            // Serde encodes a non-skipped Rust Option as null, so both null and
            // an omitted field represent the absence of a process exit code.
            if (result !== undefined && typedDisposition !== undefined && typeof bash.command === "string" && typeof result.output === "string" && typeof result.cancelled === "boolean" && typeof result.truncated === "boolean" && typeof bash.excludeFromContext === "boolean" && (result.exitCode == null || typeof result.exitCode === "number") && (result.fullOutputPath == null || typeof result.fullOutputPath === "string")) {
              actions.push({ type: "bash/upserted", bash: { messageId: messageId as MessageId, turnId: meta.turnId, command: bash.command, output: result.output, disposition: typedDisposition, ...(typeof result.exitCode === "number" ? { exitCode: result.exitCode } : {}), cancelled: result.cancelled, truncated: result.truncated, ...(typeof result.fullOutputPath === "string" ? { fullOutputPath: result.fullOutputPath } : {}), excludeFromContext: bash.excludeFromContext, timestampMs: timing?.timestampMs ?? meta.timestampMs, startedAtMs: timing?.startedAtMs ?? meta.timestampMs, endedAtMs: timing?.endedAtMs ?? meta.timestampMs }, order });
            }
          }
        } else if (event === "extension_handler_failed" && meta !== undefined && typeof payload.extension_id === "string" && typeof payload.point === "string" && typeof payload.message === "string") {
          actions.push({ type: "extension/upserted", extension: { id: `error:${meta.turnId}:${meta.sequence}`, turnId: meta.turnId, extensionId: payload.extension_id, customType: payload.point, blocks: [], message: payload.message, timestampMs: meta.timestampMs, kind: "error" }, order });
        } else if (event === "mcp_elicitation_requested" && typeof payload.request === "object" && payload.request !== null && !Array.isArray(payload.request)) {
          const request = payload.request as Record<string, unknown>;
          const context = typeof request.context === "object" && request.context !== null && !Array.isArray(request.context) ? request.context as Record<string, unknown> : undefined;
          const mode = typeof request.mode === "object" && request.mode !== null && !Array.isArray(request.mode) ? request.mode as Record<string, unknown> : undefined;
          const typedMode = mode?.type === "form" && typeof mode.message === "string"
            ? { type: "form" as const, message: mode.message, requestedSchema: mode.requestedSchema }
            : mode?.type === "url" && typeof mode.message === "string" && typeof mode.url === "string" && typeof mode.elicitationId === "string"
              ? { type: "url" as const, message: mode.message, url: mode.url, elicitationId: mode.elicitationId }
              : undefined;
          if (typeof request.requestId === "string" && context !== undefined && typedMode !== undefined && typeof context.serverId === "string" && typeof context.sessionId === "string" && typeof context.turnId === "string" && typeof context.traceId === "string" && typeof context.requestedAtMs === "string") {
            const elicitation: McpElicitation = { requestId: request.requestId, context: { serverId: context.serverId, sessionId: context.sessionId as McpElicitation["context"]["sessionId"], turnId: context.turnId as McpElicitation["context"]["turnId"], traceId: context.traceId, requestedAtMs: context.requestedAtMs as TimestampMs }, mode: typedMode };
            actions.push({ type: "mcp/elicitation-requested", request: elicitation });
          }
        } else if (event === "mcp_elicitation_resolved" && typeof payload.request_id === "string") {
          actions.push({ type: "mcp/elicitation-resolved", sessionId: notification.sessionId, requestId: payload.request_id });
        } else if (event === "retry_scheduled" && meta !== undefined && typeof payload.attempt === "number" && typeof payload.max_attempts === "number" && typeof payload.delay_ms === "number" && typeof payload.error === "string") {
          actions.push({ type: "retry/changed", retry: { type: "waiting", attempt: payload.attempt, maxAttempts: payload.max_attempts, scheduledAtMs: meta.timestampMs, delayMs: payload.delay_ms, error: payload.error } });
        } else if (event === "retry_start" && typeof payload.attempt === "number" && typeof payload.max_attempts === "number") {
          actions.push({ type: "retry/changed", retry: { type: "running", attempt: payload.attempt, maxAttempts: payload.max_attempts } });
        } else if (event === "retry_end" && typeof payload.attempt === "number" && typeof payload.success === "boolean") {
          actions.push({ type: "retry/changed", retry: { type: "finished", attempt: payload.attempt, success: payload.success, ...(typeof payload.final_error === "string" ? { finalError: payload.final_error } : {}) } });
        } else if (event === "compaction_start" && meta !== undefined) {
          const reason = this.decodeCompactionReason(payload.reason);
          if (reason !== undefined) actions.push({ type: "compaction/changed", compaction: { type: "running", reason, startedAtMs: meta.timestampMs } });
        } else if (event === "compaction_end" && meta !== undefined) {
          const reason = this.decodeCompactionReason(payload.reason);
          const outcome = typeof payload.outcome === "object" && payload.outcome !== null && !Array.isArray(payload.outcome)
            ? payload.outcome as Record<string, unknown>
            : undefined;
          if (reason !== undefined && outcome?.status === "completed" && typeof outcome.result === "object" && outcome.result !== null && !Array.isArray(outcome.result)) {
            const result = outcome.result as Record<string, unknown>;
            const usage = result.usage === undefined ? undefined : this.decodeModelUsage(result.usage);
            if (typeof result.entryId === "string" && typeof result.turnId === "string" && typeof result.summary === "string" && typeof result.tokensBefore === "string" && typeof result.startedAtMs === "string" && typeof result.endedAtMs === "string" && (result.usage === undefined || usage !== undefined)) {
              const compaction: CompactionEntity = {
                entryId: result.entryId as EntryId,
                turnId: result.turnId as TurnId,
                reason,
                summary: result.summary,
                tokensBefore: result.tokensBefore,
                ...(usage === undefined ? {} : { usage }),
                startedAtMs: result.startedAtMs as TimestampMs,
                endedAtMs: result.endedAtMs as TimestampMs
              };
              actions.push({ type: "compaction/upserted", compaction, order });
              actions.push({ type: "compaction/changed", compaction: { type: "finished", reason, entryId: compaction.entryId, endedAtMs: compaction.endedAtMs } });
              actions.push({ type: "usage/changed", usage: undefined });
            }
          } else if (reason !== undefined && outcome?.status === "failed" && typeof outcome.message === "string") {
            actions.push({ type: "compaction/changed", compaction: { type: "failed", reason, message: outcome.message, endedAtMs: meta.timestampMs } });
          } else if (reason !== undefined && outcome?.status === "cancelled") {
            actions.push({ type: "compaction/changed", compaction: { type: "cancelled", reason, endedAtMs: meta.timestampMs } });
          }
        }
      }
    } else if (!(typeof kind === "string" && kind.startsWith("_"))) {
      actions.push({ type: "diagnostic/added", message: `Unknown session update: ${String(kind)}` });
    }

    return { scope: "session", actions, refreshRuntime: false };
  }

  /** Decodes one complete ACP command snapshot while isolating malformed entries. */
  private decodeAvailableCommands(value: unknown): Readonly<{
    commands: readonly AvailableCommandEntity[] | undefined;
    diagnostics: readonly string[];
  }> {
    if (!Array.isArray(value)) {
      return {
        // A malformed envelope cannot authoritatively replace the last valid snapshot.
        commands: undefined,
        diagnostics: ["Available Commands update must contain an array"]
      };
    }
    const commands: AvailableCommandEntity[] = [];
    const diagnostics: string[] = [];
    value.forEach((item, index) => {
      if (typeof item !== "object" || item === null || Array.isArray(item)) {
        diagnostics.push(`Available command ${index} must be an object`);
        return;
      }
      const command = item as Record<string, unknown>;
      if (typeof command.name !== "string" || typeof command.description !== "string") {
        diagnostics.push(`Available command ${index} must contain string name and description`);
        return;
      }
      const commandProductMeta = AcpProtocol.productMeta(command._meta, this.namespace);
      const slash = typeof commandProductMeta?.slashCommand === "object" && commandProductMeta.slashCommand !== null && !Array.isArray(commandProductMeta.slashCommand)
        ? commandProductMeta.slashCommand as Record<string, unknown>
        : undefined;
      const source: SlashCommandSource | undefined = slash?.source === "builtin" || slash?.source === "extension" || slash?.source === "skill" || slash?.source === "prompt_template" ? slash.source : undefined;
      const qualifiedName = typeof slash?.qualifiedName === "string" ? slash.qualifiedName : undefined;
      const aliasKind: SlashCommandAliasKind | undefined = slash?.aliasKind === "canonical" || slash?.aliasKind === "short" ? slash.aliasKind : undefined;
      const metadata: Readonly<{ source?: SlashCommandSource; qualifiedName?: string; aliasKind?: SlashCommandAliasKind }> = {
        ...(source === undefined ? {} : { source }),
        ...(qualifiedName === undefined ? {} : { qualifiedName }),
        ...(aliasKind === undefined ? {} : { aliasKind })
      };
      if (command.input === undefined) {
        commands.push({ name: command.name, description: command.description, ...metadata });
        return;
      }
      if (typeof command.input !== "object" || command.input === null || Array.isArray(command.input)) {
        diagnostics.push(`Available command ${index} input must be a text input`);
        return;
      }
      const input = command.input as Record<string, unknown>;
      if (input.type !== "text" || typeof input.hint !== "string") {
        diagnostics.push(`Available command ${index} input must contain text type and string hint`);
        return;
      }
      commands.push({
        name: command.name,
        description: command.description,
        argumentHint: input.hint,
        ...metadata
      });
    });
    return { commands, diagnostics };
  }

  /** Decodes exact string token fields carried in product metadata. */
  private decodeModelUsage(value: unknown): ModelUsage | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    const usage = value as Record<string, unknown>;
    if (typeof usage.input_tokens !== "string" || typeof usage.output_tokens !== "string" || typeof usage.cache_read_tokens !== "string" || typeof usage.cache_write_tokens !== "string" || typeof usage.total_tokens !== "string" || (usage.reasoning_tokens !== undefined && typeof usage.reasoning_tokens !== "string")) return undefined;
    return { inputTokens: usage.input_tokens, outputTokens: usage.output_tokens, cacheReadTokens: usage.cache_read_tokens, cacheWriteTokens: usage.cache_write_tokens, ...(typeof usage.reasoning_tokens === "string" ? { reasoningTokens: usage.reasoning_tokens } : {}), totalTokens: usage.total_tokens };
  }

  /** Restores precision-safe persisted message timing during ACP replay. */
  private decodeMessageTiming(value: unknown): Readonly<{ timestampMs: TimestampMs; startedAtMs: TimestampMs; endedAtMs: TimestampMs }> | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    const timing = value as Record<string, unknown>;
    if (typeof timing.timestamp_ms !== "string" || typeof timing.started_at_ms !== "string" || typeof timing.ended_at_ms !== "string") return undefined;
    return { timestampMs: timing.timestamp_ms as TimestampMs, startedAtMs: timing.started_at_ms as TimestampMs, endedAtMs: timing.ended_at_ms as TimestampMs };
  }

  /** Decodes provider diagnostics attached to a complete Assistant update. */
  private decodeAssistantDiagnostics(value: unknown): AssistantDiagnostics | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    const assistant = value as Record<string, unknown>;
    const usage = this.decodeModelUsage(assistant.usage);
    if (typeof assistant.provider_id !== "string" || typeof assistant.model_id !== "string" || typeof assistant.stop_reason !== "string" || usage === undefined) return undefined;
    return { providerId: assistant.provider_id, modelId: assistant.model_id, stopReason: assistant.stop_reason, usage, ...(typeof assistant.raw_stop_reason === "string" ? { rawStopReason: assistant.raw_stop_reason } : {}), ...(typeof assistant.error === "string" ? { error: assistant.error } : {}) };
  }

  /** Decodes product Slash Command metadata without affecting baseline ACP messages. */
  private decodeSlashCommandMetadata(value: unknown): SlashCommandMessageMeta | undefined {
    if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
    const command = value as Record<string, unknown>;
    if (typeof command.name !== "string") return undefined;
    if (command.source !== "builtin" && command.source !== "extension" && command.source !== "skill" && command.source !== "prompt_template") return undefined;
    if (command.messageKind !== "invocation" && command.messageKind !== "output" && command.messageKind !== "expansion") return undefined;
    if (command.status !== undefined && command.status !== "succeeded" && command.status !== "failed") return undefined;
    return {
      name: command.name,
      source: command.source,
      messageKind: command.messageKind,
      ...(command.status === undefined ? {} : { status: command.status })
    };
  }

  /** Narrows the three persisted pi-compatible compaction reasons. */
  private decodeCompactionReason(value: unknown): CompactionReason | undefined {
    return value === "manual" || value === "threshold" || value === "overflow" ? value : undefined;
  }
}
