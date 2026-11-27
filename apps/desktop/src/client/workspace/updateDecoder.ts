import { decodeCommands } from "../acp/commands";
import { AcpProtocol } from "../acp/protocol";
import { decodeSessionConfig } from "../acp/sessionConfig";
import type { EventMeta, MessageId, SessionId, SessionUpdateNotification, TimestampMs } from "../acp/protocol";
import type { EventOrder, MessageAttachment, MessageEntity, SessionNotice, ToolCallEntity, ToolKind } from "../domain/model";
import { fileChanges } from "../fileChanges";
import { interactionUpdate, questionDecisions } from "./interactions";
import type { SessionEntities, SessionWorkspaceAction } from "./sessionState";
import type { WorkspaceAction } from "./state";
import { decodeTerminal } from "./terminal";

export type DecodedSessionUpdate =
  | Readonly<{ scope: "branch"; refreshRuntime: false }>
  | Readonly<{ scope: "global"; actions: readonly WorkspaceAction[]; refreshRuntime: false }>
  | Readonly<{ scope: "session"; actions: readonly SessionWorkspaceAction[]; refreshRuntime: boolean }>;

/** What every Session update carries besides its own fields. */
type UpdateContext = Readonly<{
  update: Readonly<Record<string, unknown>>;
  meta: EventMeta | undefined;
  productMeta: Record<string, unknown> | undefined;
  order: EventOrder;
  entities: SessionEntities;
  /** Published edits can match replayed payloads even while the new branch is still being assembled. */
  publishedTools?: SessionEntities["tools"];
}>;

const TOOL_KINDS: ReadonlySet<ToolKind> = new Set<ToolKind>(["read", "edit", "delete", "move", "search", "execute", "think", "fetch", "switch_mode", "other"]);

/** Joins the text of ACP content blocks; a value that is not a block array has no text. */
function contentText(content: unknown): string {
  if (!Array.isArray(content)) return "";
  return content.map((block: unknown) => {
    const record = AcpProtocol.record(block);
    return (record?.type === "text" || record?.type === undefined) && typeof record?.text === "string" ? record.text : "";
  }).join("");
}

/** Reuses the raw result's binary strings when the kernel also sends them in native ACP content. */
function shareMediaBytes(content: readonly unknown[] | undefined, rawOutput: unknown): readonly unknown[] | undefined {
  if (!content?.length) return content;
  const raw = AcpProtocol.record(rawOutput);
  const blocks = raw?.blocks ?? AcpProtocol.record(raw?.content)?.blocks;
  if (!Array.isArray(blocks)) return content;
  const payloads = new Map<string, string>();
  for (const item of blocks) {
    const block = AcpProtocol.record(item);
    const data = block?.type === "image" || block?.type === "audio" ? block.data
      : block?.type === "embedded_resource" ? AcpProtocol.record(block.content)?.data : undefined;
    if (typeof data === "string") payloads.set(data, data);
  }
  if (!payloads.size) return content;
  // Match by bytes, since native content can include extra diff hunks or omit kernel-only blocks.
  return content.map((item) => {
    const entry = AcpProtocol.record(item);
    const block = AcpProtocol.record(entry?.content);
    if (block?.type === "image" || block?.type === "audio") {
      const data = typeof block.data === "string" ? payloads.get(block.data) : undefined;
      if (data !== undefined) return { ...entry, content: { ...block, data } };
    }
    if (block?.type === "resource") {
      const resource = AcpProtocol.record(block.resource);
      const blob = typeof resource?.blob === "string" ? payloads.get(resource.blob) : undefined;
      if (blob !== undefined) return { ...entry, content: { ...block, resource: { ...resource, blob } } };
    }
    return item;
  });
}

/**
 * Narrows ACP wire updates into typed workspace actions without owning UI state. Only what the
 * app shows is decoded: titles, notices, settings, messages, tool calls and the run state; other updates
 * are ignored.
 */
export class SessionUpdateDecoder {
  readonly namespace: string;

  /** Creates a decoder for one product extension namespace. */
  constructor(namespace: string) {
    this.namespace = namespace;
  }

  /** Decodes one ACP notification while preserving omitted-field upsert semantics. */
  decode(notification: SessionUpdateNotification, entities: SessionEntities, receivedOrder: number, publishedTools?: SessionEntities["tools"]): DecodedSessionUpdate {
    const update = notification.update;
    const kind = update.sessionUpdate;
    if (kind === `_${this.namespace}/event` && AcpProtocol.record(update.payload)?.event === "branch_navigated") {
      return { scope: "branch", refreshRuntime: false };
    }
    if (kind === "available_commands_update") {
      const commands = decodeCommands(update.availableCommands);
      return {
        scope: "global",
        actions: commands === undefined ? [] : [{ type: "commands/replaced", sessionId: notification.sessionId, commands }],
        refreshRuntime: false,
      };
    }
    if (kind === "config_option_update") {
      const options = decodeSessionConfig(update.configOptions);
      return {
        scope: "global",
        actions: options === undefined ? [] : [{ type: "config/replaced", sessionId: notification.sessionId, options }],
        refreshRuntime: false,
      };
    }
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
      entities, publishedTools
    };
    if (kind === `_${this.namespace}/event`) {
      const event = AcpProtocol.record(update.payload);
      if ((event?.event === "run_start" || event?.event === "run_end") && typeof event.run_id === "string") {
        return { scope: "session", actions: [{ type: "run/boundary" }], refreshRuntime: false };
      }
      if (event?.event === "message_end") {
        const record = AcpProtocol.record(event.message);
        const decisions = questionDecisions(record, this.namespace);
        const actions: SessionWorkspaceAction[] = [];
        if (decisions?.length) {
          actions.push({ type: "interactions/changed", change: { type: "settled", ids: decisions.map(({ id }) => id) } });
          // Older persisted extension messages nest their identity; today's kernel flattens it.
          const messageId = record?.message_id ?? AcpProtocol.record(record?.identity)?.message_id;
          const extension = AcpProtocol.record(AcpProtocol.record(record?.content)?.extension);
          if (meta && typeof messageId === "string" && extension?.display !== false) {
            const timing = this.messageTiming(context.productMeta?.messageTiming);
            actions.push({ type: "message/upserted", order: context.order, message: {
              messageId: messageId as MessageId, role: "user", decisions, text: "", reasoning: "",
              startedAtMs: timing?.startedAtMs ?? meta.timestampMs, endedAtMs: timing?.endedAtMs ?? meta.timestampMs,
            } });
          }
        }
        return { scope: "session", actions, refreshRuntime: false };
      }
    }
    const action = this.sessionAction(kind, context);
    const actions = action === undefined ? [] : [action];
    if (kind === "tool_call_update" || kind === "plan_update" || kind === `_${this.namespace}/event`) {
      const change = interactionUpdate(update, this.namespace);
      if (change) actions.push({ type: "interactions/changed", change });
    }
    return { scope: "session", actions, refreshRuntime: false };
  }

  /** Projects messages, tools and the byte streams referenced by tool content. */
  private sessionAction(kind: unknown, context: UpdateContext): SessionWorkspaceAction | undefined {
    const { update, meta, productMeta, order } = context;
    switch (kind) {
      case "agent_message_chunk":
      case "user_message_chunk":
      case "agent_thought_chunk": {
        const content = AcpProtocol.record(update.content);
        const contentMeta = AcpProtocol.eventMeta(content?._meta, this.namespace) ?? meta;
        if (typeof update.messageId !== "string" || !content || contentMeta === undefined) return undefined;
        const messageId = update.messageId as MessageId;
        if (typeof content.type === "string" && content.type !== "text") {
          return kind === "agent_message_chunk"
            ? { type: "message/attachment-appended", messageId, block: content, meta: contentMeta, order } : undefined;
        }
        if (typeof content.text !== "string") return undefined;
        return kind === "agent_thought_chunk"
          ? { type: "message/reasoning-delta", messageId, delta: content.text, meta: contentMeta, order }
          : { type: "message/text-delta", messageId, role: kind === "user_message_chunk" ? "user" : "assistant", delta: content.text, meta: contentMeta, order };
      }
      case "user_message":
        // The accepted shell prompt opens a turn; its separate tool entity owns output and exit state.
        if (productMeta?.origin === "branch_summary") return undefined;
        return this.message(context, "user");
      case "agent_message":
        return this.message(context, "assistant");
      case "agent_thought":
        return this.thought(context);
      case "tool_call_update":
        return this.tool(context);
      case "terminal_update":
      case "terminal_output_chunk": {
        const previous = typeof update.terminalId === "string" ? context.entities.terminals.get(update.terminalId) : undefined;
        const terminal = decodeTerminal(previous, update);
        return terminal ? { type: "terminal/upserted", terminal } : undefined;
      }
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
    let imageCount = existing?.imageCount;
    let attachments = existing?.attachments;
    let text = existing?.text ?? "";
    if ("content" in update) {
      const blocks: readonly unknown[] = Array.isArray(update.content) ? update.content : [];
      text = "";
      imageCount = 0;
      const media: MessageAttachment[] = [];
      for (const value of blocks) {
        const block = AcpProtocol.record(value);
        if (!block) continue;
        if (block.type === "text" || block.type === undefined) {
          if (typeof block.text === "string") text += block.text;
        } else if (role === "assistant" && typeof block.type === "string") {
          media.push({ textOffset: text.length, block });
        } else if (block.type === "image") imageCount++;
      }
      attachments = media;
    }
    const message: MessageEntity = {
      messageId,
      role,
      ...(context.productMeta?.origin === "user_bash" || existing?.origin === "user_bash" ? { origin: "user_bash" } : {}),
      text,
      ...(role === "user" && imageCount ? { imageCount } : {}),
      ...(role === "assistant" && attachments?.length ? { attachments } : {}),
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
    let status = previous?.status ?? "pending";
    if (update.status === null) status = "pending";
    else if (update.status === "pending" || update.status === "failed" || update.status === "completed"
      || update.status === "in_progress" || update.status === "cancelled") status = update.status;
    let kind = previous?.kind;
    if (update.kind === null) kind = "other";
    else if (typeof update.kind === "string" && TOOL_KINDS.has(update.kind as ToolKind)) kind = update.kind as ToolKind;
    const name = update.name === null ? undefined : typeof update.name === "string" ? update.name : previous?.name;
    const rawInput = update.rawInput === undefined ? previous?.rawInput : update.rawInput ?? undefined;
    const rawOutput = update.rawOutput === undefined ? previous?.rawOutput : update.rawOutput ?? undefined;
    const contentPatch = Array.isArray(update.content) ? update.content : update.content === null ? [] : undefined;
    const content = contentPatch !== undefined || rawOutput !== previous?.rawOutput
      ? shareMediaBytes(contentPatch ?? previous?.content, rawOutput) : previous?.content;
    const subagentSessionId = this.subagentSessionId(update.rawOutput) ?? previous?.subagentSessionId;
    const ended = status === "completed" || status === "failed" || status === "cancelled";
    // A fresh replay can reuse a matching edit from the published view. Only replayed content restores an edit.
    const changes = previous?.changes ?? [];
    const previousEdits = changes.length ? changes : context.publishedTools?.get(toolCallId)?.changes;
    const tool: ToolCallEntity = {
      toolCallId,
      title: typeof update.title === "string" ? update.title : update.title === null ? update.toolCallId : previous?.title ?? update.toolCallId,
      status,
      ...(name === undefined ? {} : { name }),
      ...(rawInput === undefined ? {} : { rawInput }),
      ...(rawOutput === undefined ? {} : { rawOutput }),
      ...(content === undefined ? {} : { content }),
      ...(kind === undefined ? {} : { kind }),
      // A discovered child stays even when a later cancellation carries no details.
      ...(subagentSessionId === undefined ? {} : { subagentSessionId }),
      changes: contentPatch === undefined ? changes : fileChanges(toolCallId, contentPatch, previousEdits),
      ...(ended ? { endedAtMs: previous?.endedAtMs ?? meta?.timestampMs } : {})
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
