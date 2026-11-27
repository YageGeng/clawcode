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

/** An image, resource or other ACP block positioned between runs of assistant text. */
export type MessageAttachment = Readonly<{ textOffset: number; block: Readonly<Record<string, unknown>> }>;

/** One durable answer or skip. Older kernel records may omit the question title. */
export type QuestionDecision = Readonly<{ id: string; question: string; answer?: string }>;

export type MessageEntity = Readonly<{
  messageId: MessageId;
  /** Assigned on first insertion and retained when a later frame updates this message. */
  transcriptOrder?: EventOrder;
  role: "user" | "assistant";
  /** A user shell operation has its own prompt and tool and does not start an agent Run. */
  origin?: "user_bash";
  text: string;
  /** Number of images on a user turn; replay can retain the count without keeping the full base64 payload. */
  imageCount?: number;
  /** Offsets into text keep media in wire order without rebuilding previous blocks on token updates. */
  attachments?: readonly MessageAttachment[];
  /** The user's recorded choices, in the order they were settled. */
  decisions?: readonly QuestionDecision[];
  /** A user decision recorded during ongoing work stays inside the same displayed turn. */
  continuesTurn?: boolean;
  reasoning: string;
  startedAtMs: TimestampMs;
  endedAtMs: TimestampMs;
}>;

export type ToolCallEntity = Readonly<{
  toolCallId: ToolCallId;
  /** The call's position stays fixed when its result arrives. */
  transcriptOrder?: EventOrder;
  title: string;
  name?: string;
  rawInput?: unknown;
  rawOutput?: unknown;
  content?: readonly unknown[];
  status: "pending" | "in_progress" | "completed" | "failed" | "cancelled";
  kind?: ToolKind;
  /** The child Session a spawn tool started, once its result names it. */
  subagentSessionId?: SessionId;
  /** The files its `diff` content changed, counted once when the content arrives. */
  changes: readonly FileChange[];
  endedAtMs?: TimestampMs;
}>;

/** One agent-owned terminal projected by the current Session branch. */
export type TerminalEntity = Readonly<{
  terminalId: string;
  command?: string;
  cwd?: string;
  output: string;
  exitStatus?: Readonly<{ exitCode?: number; signal?: string }>;
  /** At most three bytes carried into the next chunk when UTF-8 splits across frames. */
  pendingUtf8?: Uint8Array;
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
  /** The exact Markdown proposal of a switch_mode permission request. */
  plan?: string;
  options: readonly PermissionOption[];
}>;

export type TranscriptEntry =
  | Readonly<{ type: "message"; messageId: MessageId; order: EventOrder }>
  | Readonly<{ type: "tool"; toolCallId: ToolCallId; order: EventOrder }>;
