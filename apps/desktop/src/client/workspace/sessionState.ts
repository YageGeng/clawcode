import type { EventMeta, MessageId, ToolCallId } from "../acp/protocol";
import type { EventOrder, MessageEntity, ToolCallEntity, TranscriptEntry } from "../domain/model";
import { EventOrdering } from "../domain/model";

/** What one Session shows: its messages and tool calls in transcript order, and whether a turn runs. */
export type SessionWorkspaceState = Readonly<{
  messages: ReadonlyMap<MessageId, MessageEntity>;
  transcript: readonly TranscriptEntry[];
  tools: ReadonlyMap<ToolCallId, ToolCallEntity>;
  running: boolean;
}>;

/** The entities a decoder reads to merge an update into what the Session already has. */
export type SessionEntities = Pick<SessionWorkspaceState, "messages" | "tools">;

export type SessionWorkspaceAction =
  | { readonly type: "transcript/cleared" }
  | { readonly type: "message/upserted"; readonly message: MessageEntity; readonly order: EventOrder }
  | { readonly type: "message/text-delta"; readonly messageId: MessageId; readonly role: "user" | "assistant"; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "message/reasoning-delta"; readonly messageId: MessageId; readonly delta: string; readonly meta: EventMeta; readonly order: EventOrder }
  | { readonly type: "tool/upserted"; readonly tool: ToolCallEntity; readonly order: EventOrder }
  | { readonly type: "running/changed"; readonly running: boolean };

export const initialSessionWorkspaceState: SessionWorkspaceState = {
  messages: new Map(),
  transcript: [],
  tools: new Map(),
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
  private transcript: readonly TranscriptEntry[];
  // New entries of this frame; `commit` puts them in order.
  private appended: TranscriptEntry[] = [];
  private running: boolean;
  private touched = false;

  /** Creates one frame-local draft over `base`, sharing its collections until they change. */
  constructor(base: SessionWorkspaceState) {
    this.base = base;
    this.messages = new MapDraft(base.messages);
    this.tools = new MapDraft(base.tools);
    this.transcript = base.transcript;
    this.running = base.running;
  }

  /** The entities as this frame has left them so far. */
  snapshot(): SessionEntities {
    return { messages: this.messages.read(), tools: this.tools.read() };
  }

  /** Applies one Session action. */
  apply(action: SessionWorkspaceAction): void {
    // Setting the run state it already has changes nothing, so the Session keeps its identity.
    if (action.type === "running/changed" && action.running === this.running) return;
    this.touched = true;
    switch (action.type) {
      // Run state is not transcript: a replay keeps it.
      case "transcript/cleared":
        this.messages.replace(new Map());
        this.tools.replace(new Map());
        this.transcript = [];
        this.appended = [];
        return;
      case "message/upserted": {
        const messages = this.messages.write();
        if (!messages.has(action.message.messageId)) this.appended.push({ type: "message", messageId: action.message.messageId, order: action.order });
        messages.set(action.message.messageId, action.message);
        return;
      }
      case "message/text-delta":
      case "message/reasoning-delta": {
        const messages = this.messages.write();
        const previous = messages.get(action.messageId);
        const base: MessageEntity = previous ?? {
          messageId: action.messageId,
          role: action.type === "message/text-delta" ? action.role : "assistant",
          text: "",
          reasoning: "",
          startedAtMs: action.meta.timestampMs,
          endedAtMs: action.meta.timestampMs
        };
        if (previous === undefined) this.appended.push({ type: "message", messageId: action.messageId, order: action.order });
        messages.set(action.messageId, action.type === "message/text-delta"
          ? { ...base, text: base.text + action.delta, endedAtMs: action.meta.timestampMs }
          : { ...base, reasoning: base.reasoning + action.delta, endedAtMs: action.meta.timestampMs });
        return;
      }
      case "tool/upserted": {
        const tools = this.tools.write();
        if (!tools.has(action.tool.toolCallId)) this.appended.push({ type: "tool", toolCallId: action.tool.toolCallId, order: action.order });
        tools.set(action.tool.toolCallId, action.tool);
        return;
      }
      case "running/changed":
        this.running = action.running;
        return;
    }
  }

  /** The Session after this frame; `base` itself when no action reached it. */
  commit(): SessionWorkspaceState {
    if (!this.touched) return this.base;
    return {
      messages: this.messages.read(),
      transcript: this.mergedTranscript(),
      tools: this.tools.read(),
      running: this.running
    };
  }

  /** The transcript with this frame's entries in order: appended at the end when they follow it, as live updates do. */
  private mergedTranscript(): readonly TranscriptEntry[] {
    if (this.appended.length === 0) return this.transcript;
    const appended = this.appended.sort((left, right) => EventOrdering.compare(left.order, right.order));
    const last = this.transcript.at(-1);
    if (last === undefined || EventOrdering.compare(last.order, appended[0]!.order) <= 0) return [...this.transcript, ...appended];
    return [...this.transcript, ...appended].sort((left, right) => EventOrdering.compare(left.order, right.order));
  }
}

/** Applies one action to one Session. */
export function reduceSessionWorkspace(state: SessionWorkspaceState, action: SessionWorkspaceAction): SessionWorkspaceState {
  const stager = new SessionWorkspaceStager(state);
  stager.apply(action);
  return stager.commit();
}
