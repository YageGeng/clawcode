import type { EntryId, MessageId, RunId, TurnId } from "../acp/protocol";
import type {
  AgentRunUsage,
  BashExecutionEntity,
  CompactionEntity,
  CompactionStatus,
  ContextUsage,
  ExtensionEntity,
  McpElicitation,
  McpSessionSnapshot,
  MessageEntity,
  ModelUsage,
  PendingMessages,
  RetryStatus,
  SessionEvent,
  SessionTree,
  SkillListResult,
  ToolCallEntity,
  TerminalSnapshot,
  TranscriptEntry
} from "../domain/model";
import { EventOrdering as Ordering, ModelUsages } from "../domain/model";
import { EventSequenceRanges, initialSessionWorkspaceState, MAX_RETAINED_SESSION_EVENTS, reduceSessionWorkspace } from "./sessionState";
import type { SessionWorkspaceAction, SessionWorkspaceState } from "./sessionState";

/** Retains an immutable Map until one frame action first requests a mutable view. */
class MapDraft<K, V> {
  private readonly base: ReadonlyMap<K, V>;
  private mutable: Map<K, V> | undefined;

  /** Creates a lazy frame-local Map backed by the current immutable projection. */
  constructor(base: ReadonlyMap<K, V>) {
    this.base = base;
  }

  /** Returns the current read view without forcing a collection copy. */
  read(): ReadonlyMap<K, V> {
    return this.mutable ?? this.base;
  }

  /** Returns a mutable frame view, copying the base collection at most once. */
  write(): Map<K, V> {
    this.mutable ??= new Map(this.base);
    return this.mutable;
  }

  /** Replaces the frame view after actions that reset an entire collection. */
  replace(value: Map<K, V>): void {
    this.mutable = value;
  }
}

/** Applies one transport frame to a Session with copy-on-write collections. */
export class SessionWorkspaceStager {
  private readonly messages: MapDraft<MessageId, MessageEntity>;
  private readonly tools: MapDraft<string, ToolCallEntity>;
  private readonly bashExecutions: MapDraft<MessageId, BashExecutionEntity>;
  private readonly extensions: MapDraft<string, ExtensionEntity>;
  private readonly compactions: MapDraft<EntryId, CompactionEntity>;
  private readonly turnRuns: MapDraft<TurnId, RunId>;
  private readonly runUsages: MapDraft<RunId, AgentRunUsage>;
  private readonly mcpElicitations: MapDraft<string, McpElicitation>;
  private transcript: readonly TranscriptEntry[];
  private appendedTranscript: TranscriptEntry[] = [];
  private events: readonly SessionEvent[];
  private appendedEvents: SessionEvent[] = [];
  private diagnostics: readonly string[];
  private appendedDiagnostics: string[] = [];
  private pending: PendingMessages;
  private tree: SessionTree | undefined;
  private skills: SkillListResult["skills"];
  private skillDiagnostics: SkillListResult["diagnostics"];
  private availableCommands: SessionWorkspaceState["availableCommands"];
  private mcpSnapshot: McpSessionSnapshot;
  private terminals: readonly TerminalSnapshot[];
  private terminalRevision: number;
  private terminalTargetRevision: number;
  private running: boolean;
  private contextUsage: ContextUsage | undefined;
  private retry: RetryStatus | undefined;
  private compaction: CompactionStatus;
  private outcomeUnknown: boolean;

  /** Creates one frame-local draft while preserving untouched collection identities. */
  constructor(base: SessionWorkspaceState) {
    this.messages = new MapDraft(base.messages);
    this.tools = new MapDraft(base.tools);
    this.bashExecutions = new MapDraft(base.bashExecutions);
    this.extensions = new MapDraft(base.extensions);
    this.compactions = new MapDraft(base.compactions);
    this.turnRuns = new MapDraft(base.turnRuns);
    this.runUsages = new MapDraft(base.runUsages);
    this.mcpElicitations = new MapDraft(base.mcpElicitations);
    this.transcript = base.transcript;
    this.events = base.events;
    this.diagnostics = base.diagnostics;
    this.pending = base.pending;
    this.tree = base.tree;
    this.skills = base.skills;
    this.skillDiagnostics = base.skillDiagnostics;
    this.availableCommands = base.availableCommands;
    this.mcpSnapshot = base.mcpSnapshot;
    this.terminals = base.terminals;
    this.terminalRevision = base.terminalRevision;
    this.terminalTargetRevision = base.terminalTargetRevision;
    this.running = base.running;
    this.contextUsage = base.contextUsage;
    this.retry = base.retry;
    this.compaction = base.compaction;
    this.outcomeUnknown = base.outcomeUnknown;
  }

  /** Applies one Session action using copy-on-write collections. */
  apply(action: SessionWorkspaceAction): void {
    switch (action.type) {
      case "transcript/cleared": {
        // Clearing stale entities must not hide a disconnected Run whose final
        // outcome is still unknown until a successful replay completes.
        const outcomeUnknown = this.outcomeUnknown;
        this.messages.replace(new Map());
        this.tools.replace(new Map());
        this.bashExecutions.replace(new Map());
        this.extensions.replace(new Map());
        this.compactions.replace(new Map());
        this.turnRuns.replace(new Map());
        this.runUsages.replace(new Map());
        this.mcpElicitations.replace(new Map());
        this.transcript = [];
        this.appendedTranscript = [];
        this.events = [];
        this.appendedEvents = [];
        this.diagnostics = [];
        this.appendedDiagnostics = [];
        this.pending = initialSessionWorkspaceState.pending;
        this.tree = undefined;
        this.skills = initialSessionWorkspaceState.skills;
        this.skillDiagnostics = initialSessionWorkspaceState.skillDiagnostics;
        this.availableCommands = initialSessionWorkspaceState.availableCommands;
        this.mcpSnapshot = initialSessionWorkspaceState.mcpSnapshot;
        this.terminals = initialSessionWorkspaceState.terminals;
        this.terminalRevision = initialSessionWorkspaceState.terminalRevision;
        this.terminalTargetRevision = initialSessionWorkspaceState.terminalTargetRevision;
        this.running = false;
        this.contextUsage = undefined;
        this.retry = undefined;
        this.compaction = initialSessionWorkspaceState.compaction;
        this.outcomeUnknown = outcomeUnknown;
        return;
      }
      case "message/upserted": {
        const previous = this.messages.read().get(action.message.messageId);
        this.messages.write().set(action.message.messageId, {
          ...action.message,
          sequenceRange: EventSequenceRanges.include(previous?.sequenceRange, action.order.sequence)
        });
        if (previous === undefined) {
          this.appendedTranscript.push({ type: "message", messageId: action.message.messageId, order: action.order });
        }
        return;
      }
      case "message/text-delta":
      case "message/reasoning-delta": {
        const previous = this.messages.read().get(action.messageId);
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
        this.messages.write().set(action.messageId, action.type === "message/text-delta"
          ? { ...base, text: base.text + action.delta, endedAtMs: action.meta.timestampMs, streaming: true, sequenceRange }
          : { ...base, reasoning: base.reasoning + action.delta, endedAtMs: action.meta.timestampMs, streaming: true, sequenceRange });
        if (previous === undefined) {
          this.appendedTranscript.push({ type: "message", messageId: action.messageId, order: action.order });
        }
        return;
      }
      case "tool/upserted": {
        const previous = this.tools.read().get(action.tool.toolCallId);
        this.tools.write().set(action.tool.toolCallId, {
          ...action.tool,
          sequenceRange: EventSequenceRanges.include(previous?.sequenceRange, action.order.sequence)
        });
        if (previous === undefined) {
          this.appendedTranscript.push({ type: "tool", toolCallId: action.tool.toolCallId, order: action.order });
        }
        return;
      }
      case "bash/upserted": {
        const exists = this.bashExecutions.read().has(action.bash.messageId);
        this.bashExecutions.write().set(action.bash.messageId, action.bash);
        if (!exists) {
          this.appendedTranscript.push({ type: "bash", messageId: action.bash.messageId, order: action.order });
        }
        return;
      }
      case "extension/upserted": {
        const exists = this.extensions.read().has(action.extension.id);
        this.extensions.write().set(action.extension.id, action.extension);
        if (!exists) {
          this.appendedTranscript.push({ type: "extension", extensionEventId: action.extension.id, order: action.order });
        }
        return;
      }
      case "compaction/upserted": {
        const exists = this.compactions.read().has(action.compaction.entryId);
        this.compactions.write().set(action.compaction.entryId, action.compaction);
        if (!exists) {
          this.appendedTranscript.push({ type: "compaction", entryId: action.compaction.entryId, order: action.order });
        }
        return;
      }
      case "turn/settled": {
        if (this.turnRuns.read().has(action.turnId)) return;
        const usages = [...this.messages.read().values()]
          .filter((message) => message.turnId === action.turnId && message.assistant !== undefined)
          .map((message) => message.assistant?.usage)
          .filter((usage): usage is ModelUsage => usage !== undefined);
        this.turnRuns.write().set(action.turnId, action.runId);
        if (usages.length === 0) return;
        const previous = this.runUsages.read().get(action.runId);
        // The frame-local accumulator deduplicates TurnEnd and avoids scanning
        // every message again when the complete Agent Run later settles.
        this.runUsages.write().set(action.runId, {
          usage: ModelUsages.sum(previous === undefined ? usages : [previous.usage, ...usages]),
          settled: previous?.settled ?? false
        });
        return;
      }
      case "run/settled": {
        const previous = this.runUsages.read().get(action.runId);
        if (previous === undefined || previous.settled) return;
        this.runUsages.write().set(action.runId, { ...previous, settled: true });
        this.appendedTranscript.push({ type: "agent_usage", runId: action.runId, order: action.order });
        return;
      }
      case "recovery/completed": {
        const exists = this.transcript.some((entry) => entry.type === "recovery"
          && entry.notice.recoveryId === action.notice.recoveryId)
          || this.appendedTranscript.some((entry) => entry.type === "recovery"
            && entry.notice.recoveryId === action.notice.recoveryId);
        if (!exists) {
          this.appendedTranscript.push({ type: "recovery", notice: action.notice, order: action.order });
        }
        return;
      }
      case "event/received": {
        this.appendedEvents.push(action.event);
        return;
      }
      case "queue/replaced": {
        this.pending = {
          steering: action.pending.steering.filter((item) => {
            const messageId = item.message.message_id;
            return typeof messageId !== "string" || this.messages.read().get(messageId as MessageId)?.role !== "user";
          }),
          followUp: action.pending.followUp.filter((item) => {
            const messageId = item.message.message_id;
            return typeof messageId !== "string" || this.messages.read().get(messageId as MessageId)?.role !== "user";
          })
        };
        return;
      }
      case "queue/removed": {
        this.pending = {
          steering: this.pending.steering.filter((item) => item.queueId !== action.queueId),
          followUp: this.pending.followUp.filter((item) => item.queueId !== action.queueId)
        };
        return;
      }
      case "tree/replaced": this.tree = action.tree; return;
      case "skills/replaced": this.skills = action.result.skills; this.skillDiagnostics = action.result.diagnostics; return;
      case "commands/replaced": this.availableCommands = action.commands; return;
      case "mcp/replaced":
        if (action.snapshot.revision >= this.mcpSnapshot.revision) this.mcpSnapshot = action.snapshot;
        return;
      case "mcp/elicitations-replaced":
        this.mcpElicitations.replace(new Map(action.snapshot.requests.map((request) => [request.requestId, request])));
        return;
      case "mcp/elicitation-requested":
        this.mcpElicitations.write().set(action.request.requestId, action.request);
        return;
      case "mcp/elicitation-resolved":
        this.mcpElicitations.write().delete(action.requestId);
        return;
      case "terminals/replaced":
      case "terminals/invalidated": {
        const state = reduceSessionWorkspace(this.snapshot(), action);
        this.terminals = state.terminals;
        this.terminalRevision = state.terminalRevision;
        this.terminalTargetRevision = state.terminalTargetRevision;
        return;
      }
      case "running/changed":
        if (action.running) {
          this.running = true;
        } else {
          this.running = false;
          if (this.retry?.type === "finished") this.retry = undefined;
          if (this.compaction.type === "running") this.compaction = { type: "idle" };
        }
        return;
      case "usage/changed": this.contextUsage = action.usage; return;
      case "retry/changed": this.retry = action.retry; return;
      case "compaction/changed": this.compaction = action.compaction; return;
      case "outcome/unknown": this.outcomeUnknown = action.value; return;
      case "diagnostic/added": this.appendedDiagnostics.push(action.message); return;
    }
  }

  /** Cheap read view for the decoder, which only queries messages, pending, and tools. */
  snapshot(): SessionWorkspaceState {
    // transcript/events/diagnostics are ignored by the decoder, so pass the
    // base arrays by reference without copying or sorting on every item.
    return {
      messages: this.messages.read(),
      transcript: this.transcript,
      tools: this.tools.read(),
      bashExecutions: this.bashExecutions.read(),
      extensions: this.extensions.read(),
      compactions: this.compactions.read(),
      turnRuns: this.turnRuns.read(),
      runUsages: this.runUsages.read(),
      events: this.events,
      pending: this.pending,
      tree: this.tree,
      skills: this.skills,
      skillDiagnostics: this.skillDiagnostics,
      availableCommands: this.availableCommands,
      mcpSnapshot: this.mcpSnapshot,
      mcpElicitations: this.mcpElicitations.read(),
      terminals: this.terminals,
      terminalRevision: this.terminalRevision,
      terminalTargetRevision: this.terminalTargetRevision,
      running: this.running,
      contextUsage: this.contextUsage,
      retry: this.retry,
      compaction: this.compaction,
      outcomeUnknown: this.outcomeUnknown,
      diagnostics: this.diagnostics
    };
  }

  /** Freezes the staged projection and returns the immutable Session state. */
  commit(): SessionWorkspaceState {
    const transcript = this.appendedTranscript.length === 0
      ? this.transcript
      : [...this.transcript, ...this.appendedTranscript].sort((left, right) => Ordering.compare(left.order, right.order));
    const events = this.appendedEvents.length === 0
      ? this.events
      : this.capEvents([...this.events, ...this.appendedEvents].sort((left, right) => Ordering.compare(left.order, right.order)));
    return {
      messages: this.messages.read(),
      transcript,
      tools: this.tools.read(),
      bashExecutions: this.bashExecutions.read(),
      extensions: this.extensions.read(),
      compactions: this.compactions.read(),
      turnRuns: this.turnRuns.read(),
      runUsages: this.runUsages.read(),
      events,
      pending: this.pending,
      tree: this.tree,
      skills: this.skills,
      skillDiagnostics: this.skillDiagnostics,
      availableCommands: this.availableCommands,
      mcpSnapshot: this.mcpSnapshot,
      mcpElicitations: this.mcpElicitations.read(),
      terminals: this.terminals,
      terminalRevision: this.terminalRevision,
      terminalTargetRevision: this.terminalTargetRevision,
      running: this.running,
      contextUsage: this.contextUsage,
      retry: this.retry,
      compaction: this.compaction,
      outcomeUnknown: this.outcomeUnknown,
      diagnostics: this.appendedDiagnostics.length === 0
        ? this.diagnostics
        : [...this.diagnostics, ...this.appendedDiagnostics]
    };
  }

  /** Keeps only the newest retained events after a frame merge. */
  private capEvents(events: SessionEvent[]): SessionEvent[] {
    return events.length > MAX_RETAINED_SESSION_EVENTS
      ? events.slice(events.length - MAX_RETAINED_SESSION_EVENTS)
      : events;
  }
}
