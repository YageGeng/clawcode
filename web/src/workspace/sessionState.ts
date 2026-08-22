import type { EntryId, EventMeta, MessageId, QueueId } from "../acp/protocol";
import type {
  AvailableCommandEntity,
  BashExecutionEntity,
  CompactionEntity,
  CompactionStatus,
  ContextUsage,
  EventOrder,
  EventSequenceRange,
  ExtensionEntity,
  McpElicitation,
  McpElicitationSnapshot,
  McpSessionSnapshot,
  MessageEntity,
  PendingMessages,
  RecoveryNotice,
  RetryStatus,
  SessionEvent,
  SessionTree,
  SkillListResult,
  ToolCallEntity,
  TranscriptEntry
} from "../domain/model";
import { EventOrdering as Ordering } from "../domain/model";

export type SessionWorkspaceState = Readonly<{
  messages: ReadonlyMap<MessageId, MessageEntity>;
  transcript: readonly TranscriptEntry[];
  tools: ReadonlyMap<string, ToolCallEntity>;
  bashExecutions: ReadonlyMap<MessageId, BashExecutionEntity>;
  extensions: ReadonlyMap<string, ExtensionEntity>;
  compactions: ReadonlyMap<EntryId, CompactionEntity>;
  events: readonly SessionEvent[];
  pending: PendingMessages;
  tree: SessionTree | undefined;
  skills: SkillListResult["skills"];
  skillDiagnostics: SkillListResult["diagnostics"];
  availableCommands: readonly AvailableCommandEntity[];
  mcpSnapshot: McpSessionSnapshot;
  mcpElicitations: ReadonlyMap<string, McpElicitation>;
  running: boolean;
  contextUsage: ContextUsage | undefined;
  retry: RetryStatus | undefined;
  compaction: CompactionStatus;
  outcomeUnknown: boolean;
  diagnostics: readonly string[];
}>;

export type SessionWorkspaceAction =
  | { readonly type: "transcript/cleared" }
  | { readonly type: "message/upserted"; readonly message: MessageEntity; readonly order: EventOrder }
  | { readonly type: "message/text-delta"; readonly messageId: MessageId; readonly role: "user" | "assistant"; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "message/reasoning-delta"; readonly messageId: MessageId; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "tool/upserted"; readonly tool: ToolCallEntity; readonly order: EventOrder }
  | { readonly type: "bash/upserted"; readonly bash: BashExecutionEntity; readonly order: EventOrder }
  | { readonly type: "extension/upserted"; readonly extension: ExtensionEntity; readonly order: EventOrder }
  | { readonly type: "compaction/upserted"; readonly compaction: CompactionEntity; readonly order: EventOrder }
  | { readonly type: "recovery/completed"; readonly notice: RecoveryNotice; readonly order: EventOrder }
  | { readonly type: "event/received"; readonly event: SessionEvent }
  | { readonly type: "queue/replaced"; readonly pending: PendingMessages }
  | { readonly type: "queue/removed"; readonly queueId: QueueId }
  | { readonly type: "tree/replaced"; readonly tree: SessionTree }
  | { readonly type: "skills/replaced"; readonly result: SkillListResult }
  | { readonly type: "commands/replaced"; readonly commands: readonly AvailableCommandEntity[] }
  | { readonly type: "mcp/replaced"; readonly snapshot: McpSessionSnapshot }
  | { readonly type: "mcp/elicitations-replaced"; readonly snapshot: McpElicitationSnapshot }
  | { readonly type: "mcp/elicitation-requested"; readonly request: McpElicitation }
  | { readonly type: "mcp/elicitation-resolved"; readonly requestId: string }
  | { readonly type: "running/changed"; readonly running: boolean }
  | { readonly type: "usage/changed"; readonly usage: ContextUsage | undefined }
  | { readonly type: "retry/changed"; readonly retry: RetryStatus | undefined }
  | { readonly type: "compaction/changed"; readonly compaction: CompactionStatus }
  | { readonly type: "outcome/unknown"; readonly value: boolean }
  | { readonly type: "diagnostic/added"; readonly message: string };

/** Cap on retained ACP events; the performance blueprint treats these as debug-only. */
export const MAX_RETAINED_SESSION_EVENTS = 2_000;

export const initialSessionWorkspaceState: SessionWorkspaceState = {
  messages: new Map(),
  transcript: [],
  tools: new Map(),
  bashExecutions: new Map(),
  extensions: new Map(),
  compactions: new Map(),
  events: [],
  pending: { steering: [], followUp: [] },
  tree: undefined,
  skills: [],
  skillDiagnostics: [],
  availableCommands: [],
  mcpSnapshot: { revision: 0, servers: [], catalog: { tools: [], prompts: [], resources: [], resourceTemplates: [] } },
  mcpElicitations: new Map(),
  running: false,
  contextUsage: undefined,
  retry: undefined,
  compaction: { type: "idle" },
  outcomeUnknown: false,
  diagnostics: []
};

export const EventSequenceRanges = {
  /** Extends one UI sequence range without replacing the source event order. */
  include(range: EventSequenceRange | undefined, sequence: number): EventSequenceRange {
    return {
      start: Math.min(range?.start ?? sequence, sequence),
      end: Math.max(range?.end ?? sequence, sequence)
    };
  }
} as const;

/** Applies one update to the isolated projection owned by a single Session. */
export function reduceSessionWorkspace(
  state: SessionWorkspaceState,
  action: SessionWorkspaceAction
): SessionWorkspaceState {
  switch (action.type) {
    case "transcript/cleared": return {
      ...initialSessionWorkspaceState,
      // Clearing stale entities must not hide a disconnected Run whose final
      // outcome is still unknown until a successful replay completes.
      outcomeUnknown: state.outcomeUnknown
    };
    case "message/upserted": {
      const messages = new Map(state.messages);
      const previous = messages.get(action.message.messageId);
      const exists = previous !== undefined;
      messages.set(action.message.messageId, {
        ...action.message,
        sequenceRange: EventSequenceRanges.include(previous?.sequenceRange, action.order.sequence)
      });
      const transcript = exists ? state.transcript : [...state.transcript, { type: "message" as const, messageId: action.message.messageId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order));
      return { ...state, messages, transcript };
    }
    case "message/text-delta":
    case "message/reasoning-delta": {
      const messages = new Map(state.messages);
      const previous = messages.get(action.messageId);
      const base: MessageEntity = previous ?? {
        messageId: action.messageId,
        turnId: action.meta.turnId,
        role: action.type === "message/text-delta" ? action.role : "assistant",
        text: "",
        images: [],
        reasoning: "",
        timestampMs: action.meta.timestampMs,
        startedAtMs: action.meta.timestampMs,
        endedAtMs: action.meta.timestampMs,
        streaming: true
      };
      const sequenceRange = EventSequenceRanges.include(base.sequenceRange, action.order.sequence);
      messages.set(action.messageId, action.type === "message/text-delta"
        ? { ...base, text: base.text + action.delta, endedAtMs: action.meta.timestampMs, streaming: true, sequenceRange }
        : { ...base, reasoning: base.reasoning + action.delta, endedAtMs: action.meta.timestampMs, streaming: true, sequenceRange });
      const transcript = previous === undefined ? [...state.transcript, { type: "message" as const, messageId: action.messageId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order)) : state.transcript;
      return { ...state, messages, transcript };
    }
    case "tool/upserted": {
      const tools = new Map(state.tools);
      const previous = tools.get(action.tool.toolCallId);
      const exists = previous !== undefined;
      tools.set(action.tool.toolCallId, {
        ...action.tool,
        sequenceRange: EventSequenceRanges.include(previous?.sequenceRange, action.order.sequence)
      });
      const transcript = exists ? state.transcript : [...state.transcript, { type: "tool" as const, toolCallId: action.tool.toolCallId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order));
      return { ...state, tools, transcript };
    }
    case "bash/upserted": {
      const bashExecutions = new Map(state.bashExecutions);
      const exists = bashExecutions.has(action.bash.messageId);
      bashExecutions.set(action.bash.messageId, action.bash);
      const transcript = exists ? state.transcript : [...state.transcript, { type: "bash" as const, messageId: action.bash.messageId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order));
      return { ...state, bashExecutions, transcript };
    }
    case "extension/upserted": {
      const extensions = new Map(state.extensions);
      const exists = extensions.has(action.extension.id);
      extensions.set(action.extension.id, action.extension);
      const transcript = exists ? state.transcript : [...state.transcript, { type: "extension" as const, extensionEventId: action.extension.id, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order));
      return { ...state, extensions, transcript };
    }
    case "compaction/upserted": {
      const compactions = new Map(state.compactions);
      const exists = compactions.has(action.compaction.entryId);
      compactions.set(action.compaction.entryId, action.compaction);
      const transcript = exists ? state.transcript : [...state.transcript, { type: "compaction" as const, entryId: action.compaction.entryId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order));
      return { ...state, compactions, transcript };
    }
    case "recovery/completed": {
      const exists = state.transcript.some((entry) => entry.type === "recovery"
        && entry.notice.recoveryId === action.notice.recoveryId);
      if (exists) return state;
      return {
        ...state,
        transcript: [...state.transcript, { type: "recovery" as const, notice: action.notice, order: action.order }]
          .sort((left, right) => Ordering.compare(left.order, right.order))
      };
    }
    case "event/received": {
      const retained = state.events.length >= MAX_RETAINED_SESSION_EVENTS
        ? state.events.slice(state.events.length - (MAX_RETAINED_SESSION_EVENTS - 1))
        : state.events;
      return { ...state, events: [...retained, action.event].sort((left, right) => Ordering.compare(left.order, right.order)) };
    }
    case "queue/replaced": return {
      ...state,
      // A delayed snapshot must not reintroduce a queue item whose user
      // message has already entered the transcript.
      pending: {
        steering: action.pending.steering.filter((item) => {
          const messageId = item.message.message_id;
          return typeof messageId !== "string"
            || state.messages.get(messageId as MessageId)?.role !== "user";
        }),
        followUp: action.pending.followUp.filter((item) => {
          const messageId = item.message.message_id;
          return typeof messageId !== "string"
            || state.messages.get(messageId as MessageId)?.role !== "user";
        })
      }
    };
    case "queue/removed": return {
      ...state,
      pending: {
        steering: state.pending.steering.filter((item) => item.queueId !== action.queueId),
        followUp: state.pending.followUp.filter((item) => item.queueId !== action.queueId)
      }
    };
    case "tree/replaced": return { ...state, tree: action.tree };
    case "skills/replaced": return { ...state, skills: action.result.skills, skillDiagnostics: action.result.diagnostics };
    case "commands/replaced": return { ...state, availableCommands: action.commands };
    case "mcp/replaced": return action.snapshot.revision < state.mcpSnapshot.revision
      ? state
      : { ...state, mcpSnapshot: action.snapshot };
    case "mcp/elicitations-replaced": return {
      ...state,
      mcpElicitations: new Map(action.snapshot.requests.map((request) => [request.requestId, request]))
    };
    case "mcp/elicitation-requested": {
      const mcpElicitations = new Map(state.mcpElicitations);
      mcpElicitations.set(action.request.requestId, action.request);
      return { ...state, mcpElicitations };
    }
    case "mcp/elicitation-resolved": {
      const mcpElicitations = new Map(state.mcpElicitations);
      mcpElicitations.delete(action.requestId);
      return { ...state, mcpElicitations };
    }
    case "running/changed": return action.running
      ? { ...state, running: true }
      : {
          ...state,
          running: false,
          retry: state.retry?.type === "finished" ? state.retry : undefined,
          compaction: state.compaction.type === "running" ? { type: "idle" } : state.compaction
        };
    case "usage/changed": return { ...state, contextUsage: action.usage };
    case "retry/changed": return { ...state, retry: action.retry };
    case "compaction/changed": return { ...state, compaction: action.compaction };
    case "outcome/unknown": return { ...state, outcomeUnknown: action.value };
    case "diagnostic/added": return { ...state, diagnostics: [...state.diagnostics, action.message] };
  }
}
