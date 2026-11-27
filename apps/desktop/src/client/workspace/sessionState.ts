import type { EventMeta, MessageId, ToolCallId } from "../acp/protocol";
import type { EventOrder, MessageEntity, TerminalEntity, ToolCallEntity, TranscriptEntry } from "../domain/model";
import { EventOrdering } from "../domain/model";
import { initialInteractions, InteractionDraft } from "./interactions";
import type { InteractionChange, InteractionState } from "./interactions";

/** What one Session shows: its messages and tool calls in transcript order, and whether a turn runs. */
export type SessionWorkspaceState = Readonly<{
  interactions: InteractionState;
  /** A new object invalidates the final turn. A new history identity invalidates earlier folds too. */
  turnLayout: Readonly<{ history: symbol; lastUser?: EventOrder }>;
  /** A question answer may steer existing work or be the first input of a new Run. */
  hasRunInput: boolean;
  messages: ReadonlyMap<MessageId, MessageEntity>;
  transcript: readonly TranscriptEntry[];
  tools: ReadonlyMap<ToolCallId, ToolCallEntity>;
  terminals: ReadonlyMap<string, TerminalEntity>;
  running: boolean;
}>;

/** The entities a decoder reads to merge an update into what the Session already has. */
export type SessionEntities = Pick<SessionWorkspaceState, "messages" | "tools" | "terminals">;

export type SessionWorkspaceAction =
  | { readonly type: "interactions/changed"; readonly change: InteractionChange }
  | { readonly type: "transcript/cleared" }
  | { readonly type: "message/upserted"; readonly message: MessageEntity; readonly order: EventOrder }
  | { readonly type: "message/text-delta"; readonly messageId: MessageId; readonly role: "user" | "assistant"; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "message/reasoning-delta"; readonly messageId: MessageId; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "message/attachment-appended"; readonly messageId: MessageId; readonly block: Readonly<Record<string, unknown>>; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "tool/upserted"; readonly tool: ToolCallEntity; readonly order: EventOrder }
  | { readonly type: "terminal/upserted"; readonly terminal: TerminalEntity }
  | { readonly type: "run/boundary" }
  | { readonly type: "running/changed"; readonly running: boolean };

export const initialSessionWorkspaceState: SessionWorkspaceState = {
  interactions: initialInteractions,
  turnLayout: { history: Symbol() },
  hasRunInput: false,
  messages: new Map(),
  transcript: [],
  tools: new Map(),
  terminals: new Map(),
  running: false
};

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

  /** Replaces the frame view after an action that resets the whole collection. */
  replace(value: Map<K, V>): void {
    this.mutable = value;
  }
}

/**
 * Applies a transport frame's actions to one Session, copying each collection at most once
 * per frame instead of once per action. The only implementation of the Session actions:
 * `reduceSessionWorkspace` runs a single action through it.
 */
export class SessionWorkspaceStager {
  private readonly base: SessionWorkspaceState;
  private readonly messages: MapDraft<MessageId, MessageEntity>;
  private readonly tools: MapDraft<ToolCallId, ToolCallEntity>;
  private readonly terminals: MapDraft<string, TerminalEntity>;
  private transcript: readonly TranscriptEntry[];
  // New entries of this frame; `commit` puts them in order.
  private appended: TranscriptEntry[] = [];
  private running: boolean;
  private interactions: InteractionDraft;
  private touched = false;
  private tailChanged = false;
  private historyChanged = false;
  private roleChanged = false;
  private lastUser?: EventOrder;
  private hasRunInput: boolean;

  /** Creates one frame-local draft over `base`, sharing its collections until they change. */
  constructor(base: SessionWorkspaceState) {
    this.base = base;
    this.lastUser = base.turnLayout.lastUser;
    this.hasRunInput = base.hasRunInput;
    this.messages = new MapDraft(base.messages);
    this.tools = new MapDraft(base.tools);
    this.terminals = new MapDraft(base.terminals);
    this.transcript = base.transcript;
    this.running = base.running;
    this.interactions = new InteractionDraft(base.interactions);
  }

  /** Reports whether a resumed branch needs the host's question reservations before publication. */
  hasQuestions(): boolean {
    return this.interactions.state.questions.length > 0;
  }

  /** The entities as this frame has left them so far. */
  snapshot(): SessionEntities {
    return { messages: this.messages.read(), tools: this.tools.read(), terminals: this.terminals.read() };
  }

  /** Applies one Session action. */
  apply(action: SessionWorkspaceAction): void {
    // Setting the run state it already has changes nothing, so the Session keeps its identity.
    if (action.type === "running/changed" && action.running === this.running) return;
    if (action.type === "run/boundary" && !this.hasRunInput) return;
    if (action.type === "interactions/changed") {
      const before = this.interactions.state;
      this.interactions.apply(action.change);
      if (before !== this.interactions.state) this.touched = true;
      return;
    }
    this.touched = true;
    switch (action.type) {
      // Run state is not transcript: a replay keeps it.
      case "transcript/cleared":
        this.interactions = new InteractionDraft(initialInteractions);
        this.messages.replace(new Map());
        this.tools.replace(new Map());
        this.terminals.replace(new Map());
        this.transcript = [];
        this.appended = [];
        this.historyChanged = true;
        this.lastUser = undefined;
        this.hasRunInput = false;
        return;
      case "message/upserted": {
        let message = action.message;
        const previous = this.messages.read().get(message.messageId);
        if (message.decisions && (previous ? previous.continuesTurn : this.hasRunInput)) {
          // A repeated durable frame keeps its first position even after a Run boundary.
          message = { ...message, continuesTurn: true };
        }
        this.putMessage(message, previous, action.order);
        return;
      }
      case "message/text-delta":
      case "message/attachment-appended":
      case "message/reasoning-delta": {
        const previous = this.messages.read().get(action.messageId);
        const base: MessageEntity = previous ?? {
          messageId: action.messageId,
          role: action.type === "message/text-delta" ? action.role : "assistant",
          text: "",
          reasoning: "",
          startedAtMs: action.meta.timestampMs,
          endedAtMs: action.meta.timestampMs
        };
        const patch = action.type === "message/attachment-appended"
          ? { attachments: [...(base.attachments ?? []), { textOffset: base.text.length, block: action.block }] }
          : action.type === "message/text-delta" ? { text: base.text + action.delta } : { reasoning: base.reasoning + action.delta };
        this.putMessage({ ...base, ...patch, endedAtMs: action.meta.timestampMs }, previous, action.order);
        return;
      }
      case "terminal/upserted":
        this.terminals.write().set(action.terminal.terminalId, action.terminal);
        return;
      case "tool/upserted": {
        const tools = this.tools.write();
        const tool = action.tool;
        const previous = tools.get(tool.toolCallId);
        const order = previous?.transcriptOrder ?? action.order;
        if (!previous) this.appended.push({ type: "tool", toolCallId: tool.toolCallId, order });
        else {
          const changedFiles = previous.changes !== tool.changes && (previous.changes.length > 0 || tool.changes.length > 0);
          this.invalidateFold(order, changedFiles, previous.endedAtMs !== tool.endedAtMs);
        }
        tools.set(tool.toolCallId, { ...tool, transcriptOrder: order });
        return;
      }
      case "running/changed":
        this.running = action.running;
        return;
      case "run/boundary":
        this.hasRunInput = false;
        return;
    }
  }

  /** Only visibility, role and completed timings affect grouping. Growing visible text stays in its entity. */
  private putMessage(message: MessageEntity, previous: MessageEntity | undefined, order: EventOrder): void {
    const origin = previous?.transcriptOrder ?? order;
    if (!previous) {
      this.appended.push({ type: "message", messageId: message.messageId, order: origin });
      // Idle skips do not start work. User input and assistant messages establish an ongoing conversation.
      if (message.origin === "user_bash") this.hasRunInput = false;
      else if (!message.decisions || message.decisions.some((item) => item.answer !== undefined)) this.hasRunInput = true;
      if (message.role === "user" && !message.continuesTurn && (!this.lastUser || EventOrdering.compare(origin, this.lastUser) > 0)) {
        this.lastUser = origin;
      }
    } else {
      if (previous.role !== message.role || previous.continuesTurn !== message.continuesTurn) this.roleChanged = true;
      const hadAnswer = Boolean(previous.text || previous.attachments?.length);
      const hasAnswer = Boolean(message.text || message.attachments?.length);
      this.invalidateFold(origin, hadAnswer !== hasAnswer || !!previous.reasoning !== !!message.reasoning,
        previous.startedAtMs !== message.startedAtMs || previous.endedAtMs !== message.endedAtMs);
    }
    this.messages.write().set(message.messageId, message.transcriptOrder === origin ? message : { ...message, transcriptOrder: origin });
  }

  /** The running tail has no duration yet. Earlier turns also depend on their recorded end times. */
  private invalidateFold(origin: EventOrder, shapeChanged: boolean, timingChanged: boolean): void {
    const boundary = this.base.turnLayout.lastUser;
    const historical = boundary !== undefined && EventOrdering.compare(origin, boundary) < 0;
    if (shapeChanged || (timingChanged && (historical || !this.running))) {
      this.tailChanged = true;
      if (historical) this.historyChanged = true;
    }
  }

  /** The Session after this frame; `base` itself when no action reached it. */
  commit(): SessionWorkspaceState {
    if (!this.touched) return this.base;
    const transcript = this.mergedTranscript();
    let turnLayout = this.base.turnLayout;
    if (this.roleChanged) {
      // Reclassifying a message can move the boundary between two already published turns.
      this.lastUser = transcript.findLast((entry) => {
        const message = entry.type === "message" ? this.messages.read().get(entry.messageId) : undefined;
        return message?.role === "user" && !message.continuesTurn;
      })?.order;
      this.historyChanged = true;
    }
    if (this.historyChanged || this.tailChanged || this.appended.length) {
      turnLayout = { history: this.historyChanged ? Symbol() : turnLayout.history, lastUser: this.lastUser };
    }
    return {
      turnLayout,
      hasRunInput: this.hasRunInput,
      messages: this.messages.read(),
      transcript,
      interactions: this.interactions.commit(),
      tools: this.tools.read(),
      terminals: this.terminals.read(),
      running: this.running
    };
  }

  /** The transcript with this frame's entries in order: appended at the end when they follow it, as live updates do. */
  private mergedTranscript(): readonly TranscriptEntry[] {
    if (this.appended.length === 0) return this.transcript;
    const appended = this.appended.sort((left, right) => EventOrdering.compare(left.order, right.order));
    const last = this.transcript.at(-1);
    // A fresh replay and an insertion into an old turn both replace the cached prefix.
    if (!last) { this.historyChanged = true; return appended; }
    if (EventOrdering.compare(last.order, appended[0]!.order) <= 0) return [...this.transcript, ...appended];
    this.historyChanged = true;
    return [...this.transcript, ...appended].sort((left, right) => EventOrdering.compare(left.order, right.order));
  }
}

/** Applies one action to one Session. */
export function reduceSessionWorkspace(state: SessionWorkspaceState, action: SessionWorkspaceAction): SessionWorkspaceState {
  const stager = new SessionWorkspaceStager(state);
  stager.apply(action);
  return stager.commit();
}
