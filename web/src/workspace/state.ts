import type { EventMeta, MessageId, SessionId } from "../acp/protocol";
import type {
  CompactionStatus,
  ContextUsage,
  EventOrder,
  McpServerInfo,
  MessageEntity,
  PendingMessages,
  SessionEvent,
  SessionSummary,
  SessionTree,
  SkillInfo,
  ToolCallEntity,
  TranscriptEntry,
  RetryStatus
} from "../domain/model";
import { EventOrdering as Ordering } from "../domain/model";

export type ConnectionState =
  | { readonly type: "disconnected" }
  | { readonly type: "connecting"; readonly attempt: number }
  | { readonly type: "initializing" }
  | { readonly type: "ready" }
  | { readonly type: "reconnecting"; readonly attempt: number }
  | { readonly type: "failed"; readonly message: string };

export type WorkspaceState = Readonly<{
  connection: ConnectionState;
  sessions: readonly SessionSummary[];
  activeSessionId: SessionId | undefined;
  messages: ReadonlyMap<MessageId, MessageEntity>;
  transcript: readonly TranscriptEntry[];
  tools: ReadonlyMap<string, ToolCallEntity>;
  events: readonly SessionEvent[];
  pending: PendingMessages;
  tree: SessionTree | undefined;
  skills: readonly SkillInfo[];
  mcpServers: readonly McpServerInfo[];
  running: boolean;
  contextUsage: ContextUsage | undefined;
  retry: RetryStatus | undefined;
  compaction: CompactionStatus;
  outcomeUnknown: boolean;
  diagnostics: readonly string[];
}>;

export type WorkspaceAction =
  | { readonly type: "connection/changed"; readonly connection: ConnectionState }
  | { readonly type: "session/listed"; readonly sessions: readonly SessionSummary[] }
  | { readonly type: "session/activated"; readonly sessionId: SessionId }
  | { readonly type: "session/deactivated" }
  | { readonly type: "session/title"; readonly sessionId: SessionId; readonly title: string }
  | { readonly type: "transcript/cleared" }
  | { readonly type: "message/upserted"; readonly message: MessageEntity; readonly order: EventOrder }
  | { readonly type: "message/text-delta"; readonly messageId: MessageId; readonly role: "user" | "assistant"; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "message/reasoning-delta"; readonly messageId: MessageId; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "tool/upserted"; readonly tool: ToolCallEntity; readonly order: EventOrder }
  | { readonly type: "event/received"; readonly event: SessionEvent }
  | { readonly type: "queue/replaced"; readonly pending: PendingMessages }
  | { readonly type: "tree/replaced"; readonly tree: SessionTree }
  | { readonly type: "skills/replaced"; readonly skills: readonly SkillInfo[] }
  | { readonly type: "mcp/replaced"; readonly servers: readonly McpServerInfo[] }
  | { readonly type: "running/changed"; readonly running: boolean }
  | { readonly type: "usage/changed"; readonly usage: ContextUsage | undefined }
  | { readonly type: "retry/changed"; readonly retry: RetryStatus | undefined }
  | { readonly type: "compaction/changed"; readonly compaction: CompactionStatus }
  | { readonly type: "outcome/unknown"; readonly value: boolean }
  | { readonly type: "diagnostic/added"; readonly message: string };

export const initialWorkspaceState: WorkspaceState = {
  connection: { type: "disconnected" },
  sessions: [],
  activeSessionId: undefined,
  messages: new Map(),
  transcript: [],
  tools: new Map(),
  events: [],
  pending: { steering: [], followUp: [] },
  tree: undefined,
  skills: [],
  mcpServers: [],
  running: false,
  contextUsage: undefined,
  retry: undefined,
  compaction: { type: "idle" },
  outcomeUnknown: false,
  diagnostics: []
};

export function reduceWorkspace(
  state: WorkspaceState,
  action: WorkspaceAction
): WorkspaceState {
  switch (action.type) {
    case "connection/changed": return { ...state, connection: action.connection };
    case "session/listed": return { ...state, sessions: action.sessions };
    case "session/activated": return { ...state, activeSessionId: action.sessionId };
    case "session/deactivated": {
      return {
        ...state,
        activeSessionId: undefined,
        messages: new Map(),
        transcript: [],
        tools: new Map(),
        events: [],
        pending: { steering: [], followUp: [] },
        tree: undefined,
        mcpServers: [],
        running: false,
        contextUsage: undefined,
        retry: undefined,
        compaction: { type: "idle" },
        outcomeUnknown: false
      };
    }
    case "session/title": return {
      ...state,
      sessions: state.sessions.map((session) => session.sessionId === action.sessionId ? { ...session, title: action.title } : session)
    };
    case "transcript/cleared": return { ...state, messages: new Map(), transcript: [], tools: new Map(), events: [], contextUsage: undefined, retry: undefined, compaction: { type: "idle" } };
    case "message/upserted": {
      const messages = new Map(state.messages);
      const exists = messages.has(action.message.messageId);
      messages.set(action.message.messageId, action.message);
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
        reasoning: "",
        timestampMs: action.meta.timestampMs,
        startedAtMs: action.meta.timestampMs,
        endedAtMs: action.meta.timestampMs,
        streaming: true
      };
      messages.set(action.messageId, action.type === "message/text-delta" ? { ...base, text: base.text + action.delta, endedAtMs: action.meta.timestampMs, streaming: true } : { ...base, reasoning: base.reasoning + action.delta, endedAtMs: action.meta.timestampMs, streaming: true });
      const transcript = previous === undefined ? [...state.transcript, { type: "message" as const, messageId: action.messageId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order)) : state.transcript;
      return { ...state, messages, transcript };
    }
    case "tool/upserted": {
      const tools = new Map(state.tools);
      const exists = tools.has(action.tool.toolCallId);
      tools.set(action.tool.toolCallId, action.tool);
      const transcript = exists ? state.transcript : [...state.transcript, { type: "tool" as const, toolCallId: action.tool.toolCallId, order: action.order }]
        .sort((left, right) => Ordering.compare(left.order, right.order));
      return { ...state, tools, transcript };
    }
    case "event/received": return { ...state, events: [...state.events, action.event].sort((left, right) => Ordering.compare(left.order, right.order)) };
    case "queue/replaced": return { ...state, pending: action.pending };
    case "tree/replaced": return { ...state, tree: action.tree };
    case "skills/replaced": return { ...state, skills: action.skills };
    case "mcp/replaced": return { ...state, mcpServers: action.servers };
    case "running/changed": return action.running
      ? { ...state, running: true }
      : {
          ...state,
          running: false,
          retry: state.retry?.type === "finished" ? state.retry : undefined,
          compaction: state.compaction.type === "finished" ? state.compaction : { type: "idle" }
        };
    case "usage/changed": return { ...state, contextUsage: action.usage };
    case "retry/changed": return { ...state, retry: action.retry };
    case "compaction/changed": return { ...state, compaction: action.compaction };
    case "outcome/unknown": return { ...state, outcomeUnknown: action.value };
    case "diagnostic/added": return { ...state, diagnostics: [...state.diagnostics, action.message] };
  }
}
