import type { MessageId, SessionId, TimestampMs, ToolCallId } from "../acp/protocol";

/** One listed Session as the app shows it. */
export type SessionSummary = Readonly<{
  sessionId: SessionId;
  cwd: string;
  title: string;
  createdAtMs?: TimestampMs;
  modifiedAtMs?: TimestampMs;
}>;

/** Where an update sits in a Session: the kernel sequence, ties broken by arrival. */
export type EventOrder = Readonly<{
  sequence: number;
  receivedOrder: number;
}>;

export const EventOrdering = {
  /** Preserves ACP v2 update application order across replay and live delivery. */
  compare(left: EventOrder, right: EventOrder): number {
    return left.sequence - right.sequence || left.receivedOrder - right.receivedOrder;
  }
} as const;

export type MessageEntity = Readonly<{
  messageId: MessageId;
  role: "user" | "assistant";
  text: string;
  reasoning: string;
  startedAtMs: TimestampMs;
  endedAtMs: TimestampMs;
}>;

export type ToolCallEntity = Readonly<{
  toolCallId: ToolCallId;
  title: string;
  status: "pending" | "in_progress" | "completed" | "failed" | "cancelled";
  kind?: ToolKind;
  /** The child Session a spawn tool started, once its result names it. */
  subagentSessionId?: SessionId;
  /** The files its `diff` content changed, counted once when the content arrives. */
  changes: readonly FileChange[];
  endedAtMs?: TimestampMs;
}>;

/** One file a tool changed, from its diff content. */
export type FileChange = Readonly<{ path: string; added: number; removed: number }>;

/** ACP tool kinds the agent reports; drives the tool group's icon and summary. */
export type ToolKind = "read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch" | "switch_mode" | "other";

/** One live-only Session warning shown as a toast. */
export type SessionNotice = Readonly<{
  id: string;
  sessionId: SessionId;
  title: string;
  description?: string;
}>;

export type SessionRuntimeSnapshot = Readonly<{
  sessionId: SessionId;
  running: boolean;
}>;

/** One option the agent offers in a `session/request_permission`. */
export type PermissionOption = Readonly<{ optionId: string; name: string; kind: string }>;

/** One standard `session/request_permission` waiting for the user, oldest first. */
export type PermissionPrompt = Readonly<{
  key: string;
  sessionId: SessionId;
  title: string;
  description?: string;
  options: readonly PermissionOption[];
}>;

export type TranscriptEntry =
  | Readonly<{ type: "message"; messageId: MessageId; order: EventOrder }>
  | Readonly<{ type: "tool"; toolCallId: ToolCallId; order: EventOrder }>;
