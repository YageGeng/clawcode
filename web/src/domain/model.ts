import type {
  EntryId,
  EventMeta,
  MessageId,
  QueueId,
  SessionId,
  TimestampMs,
  ToolCallId,
  TurnId
} from "../acp/protocol";

export type SessionSummary = Readonly<{
  sessionId: SessionId;
  cwd: string;
  title: string;
  createdAtMs?: TimestampMs;
  modifiedAtMs?: TimestampMs;
  parentSessionId?: SessionId;
}>;

export type EventOrder = Readonly<{
  sequence: number;
  receivedOrder: number;
}>;

export const EventOrdering = {
  /** Preserves ACP v2 update application order across replay and live WebSocket delivery. */
  compare(left: EventOrder, right: EventOrder): number {
    return left.sequence - right.sequence || left.receivedOrder - right.receivedOrder;
  }
} as const;

export type ModelUsage = Readonly<{
  inputTokens: string;
  outputTokens: string;
  cacheReadTokens: string;
  cacheWriteTokens: string;
  reasoningTokens?: string;
  totalTokens: string;
}>;

export type AssistantDiagnostics = Readonly<{
  providerId: string;
  modelId: string;
  stopReason: string;
  rawStopReason?: string;
  usage: ModelUsage;
  error?: string;
}>;

export type ContextUsage = Readonly<{
  used: number;
  size: number;
  exact: ModelUsage;
  updatedAtMs: TimestampMs;
}>;

export type RetryStatus =
  | Readonly<{ type: "waiting"; attempt: number; maxAttempts: number; scheduledAtMs: TimestampMs; delayMs: number; error: string }>
  | Readonly<{ type: "running"; attempt: number; maxAttempts: number }>
  | Readonly<{ type: "finished"; attempt: number; success: boolean; finalError?: string }>;

export type CompactionReason = "manual" | "threshold" | "overflow";

export type CompactionStatus =
  | Readonly<{ type: "idle" }>
  | Readonly<{ type: "running"; reason: CompactionReason; startedAtMs: TimestampMs }>
  | Readonly<{ type: "finished"; reason: CompactionReason; endedAtMs: TimestampMs }>;

export type MessageEntity = Readonly<{
  messageId: MessageId;
  turnId: TurnId;
  role: "user" | "assistant" | "system";
  text: string;
  reasoning: string;
  timestampMs: TimestampMs;
  startedAtMs: TimestampMs;
  endedAtMs: TimestampMs;
  streaming: boolean;
  assistant?: AssistantDiagnostics;
}>;

export type ToolCallEntity = Readonly<{
  toolCallId: ToolCallId;
  title: string;
  status: "pending" | "in_progress" | "completed" | "failed" | "blocked";
  rawInput?: unknown;
  rawOutput?: unknown;
  content: readonly unknown[];
  startedAtMs?: TimestampMs;
  endedAtMs?: TimestampMs;
  meta?: EventMeta;
}>;

export type BashExecutionEntity = Readonly<{
  messageId: MessageId;
  turnId: TurnId;
  command: string;
  output: string;
  disposition: Readonly<{ type: "completed" }> | Readonly<{ type: "blocked"; reason: string }>;
  exitCode?: number;
  cancelled: boolean;
  truncated: boolean;
  fullOutputPath?: string;
  excludeFromContext: boolean;
  timestampMs: TimestampMs;
  startedAtMs: TimestampMs;
  endedAtMs: TimestampMs;
}>;

export type ExtensionEntity = Readonly<{
  id: string;
  turnId: TurnId;
  extensionId: string;
  customType: string;
  blocks: readonly unknown[];
  details?: unknown;
  message?: string;
  timestampMs: TimestampMs;
  kind: "message" | "error";
}>;

export type PromptResourceLink = Readonly<{
  name: string;
  uri: string;
}>;

export type PromptInput = Readonly<{
  text: string;
  resources: readonly PromptResourceLink[];
}>;

export type QueuedMessage = Readonly<{
  queueId: QueueId;
  kind: "steering" | "followUp";
  message: Readonly<Record<string, unknown>>;
}>;

export type PendingMessages = Readonly<{
  steering: readonly QueuedMessage[];
  followUp: readonly QueuedMessage[];
}>;

export type SessionTree = Readonly<{
  sessionId: SessionId;
  lane: string;
  leafId?: EntryId | null;
  entries: readonly SessionTreeEntry[];
  name?: string | null;
}>;

export type SessionTreeEntry = Readonly<{
  entryId: EntryId;
  parentId?: EntryId | null;
  kind: string;
  timestampMs: TimestampMs;
  payload: Readonly<Record<string, unknown>>;
}>;

export type SkillInfo = Readonly<{
  name: string;
  description: string;
  path: string;
  referenceDir: string;
  source: SkillSource;
  disableModelInvocation: boolean;
}>;

export type SkillSource = Readonly<{
  kind: "configured" | "pi" | "agents" | "extension";
  scope: "configured" | "project" | "user" | "extension";
  root: string;
  originBaseDir: string;
  discoveryMode: "pi" | "agents";
}>;

export type SkillCollision = Readonly<{
  name: string;
  winnerPath: string;
  loserPath: string;
}>;

export type SkillDiagnostic = Readonly<{
  severity: "warning" | "error";
  code:
    | "path_not_found"
    | "unsupported_path"
    | "file_info_failed"
    | "directory_read_failed"
    | "file_read_failed"
    | "frontmatter_invalid"
    | "metadata_invalid"
    | "name_collision"
    | "selection_rule_invalid";
  message: string;
  path?: string;
  source?: SkillSource;
  collision?: SkillCollision;
}>;

export type SkillListResult = Readonly<{
  skills: readonly SkillInfo[];
  diagnostics: readonly SkillDiagnostic[];
}>;

export type AvailableCommandEntity = Readonly<{
  name: string;
  description: string;
  argumentHint?: string;
}>;

export type McpToolInfo = Readonly<{
  reference: Readonly<{ serverId: string; remoteName: string }>;
  publicName: string;
  description?: string;
  inputSchema: unknown;
}>;

export type McpArgumentInfo = Readonly<{
  name: string;
  description?: string;
  required: boolean;
}>;

export type McpPromptInfo = Readonly<{
  reference: Readonly<{ serverId: string; remoteName: string }>;
  title?: string;
  description?: string;
  arguments: readonly McpArgumentInfo[];
}>;

export type McpResourceInfo = Readonly<{
  reference: Readonly<{ serverId: string; remoteUri: string }>;
  name: string;
  title?: string;
  description?: string;
  mimeType?: string;
  size?: number;
}>;

export type McpResourceTemplateInfo = Readonly<{
  serverId: string;
  uriTemplate: string;
  name: string;
  title?: string;
  description?: string;
  mimeType?: string;
}>;

export type McpPromptResult = Readonly<{
  description?: string;
  messages: readonly Readonly<{ role: "user" | "assistant" | "system"; content: unknown }>[];
}>;

export type McpResourceResult = Readonly<{
  contents: readonly unknown[];
}>;

export type McpCompletionResult = Readonly<{
  values: readonly string[];
  total?: number;
  hasMore: boolean;
}>;

export type McpOAuthState = "notConfigured" | "unauthenticated" | "authorizing" | "ready" | "refreshRequired" | "failed";

export type McpElicitation = Readonly<{
  requestId: string;
  context: Readonly<{
    serverId: string;
    sessionId: SessionId;
    turnId: TurnId;
    traceId: string;
    requestedAtMs: TimestampMs;
  }>;
  mode:
    | Readonly<{ type: "form"; message: string; requestedSchema: unknown }>
    | Readonly<{ type: "url"; message: string; url: string; elicitationId: string }>;
}>;

export type McpElicitationSnapshot = Readonly<{
  sessionId: SessionId;
  requests: readonly McpElicitation[];
}>;

export type McpServerStatus = Readonly<{
  serverId: string;
  protocol: "2025-11-25" | "2026-07-28";
  transport: "stdio" | "streamableHttp";
  state: "disabled" | "starting" | "negotiating" | "discovering" | "ready" | "degraded" | "failed" | "stopping" | "stopped";
  implementation?: Readonly<{ name: string; version: string }>;
  revisions: Readonly<{ tools: number; prompts: number; resources: number; resourceTemplates: number }>;
  counts: Readonly<{ tools: number; prompts: number; resources: number; resourceTemplates: number }>;
  failure?: Readonly<{ stage: string; message: string; occurredAtMs: TimestampMs }>;
  oauth: Readonly<{ state: McpOAuthState; scopes: readonly string[]; expiresAtMs?: TimestampMs; authorizationUrl?: string }>;
  updatedAtMs: TimestampMs;
}>;

export type McpSessionSnapshot = Readonly<{
  revision: number;
  servers: readonly McpServerStatus[];
  catalog: Readonly<{
    tools: readonly McpToolInfo[];
    prompts: readonly McpPromptInfo[];
    resources: readonly McpResourceInfo[];
    resourceTemplates: readonly McpResourceTemplateInfo[];
  }>;
}>;

export type SessionEvent = Readonly<{
  type: string;
  payload: Readonly<Record<string, unknown>>;
  order: EventOrder;
  meta?: EventMeta;
}>;

export type TranscriptEntry =
  | Readonly<{ type: "message"; messageId: MessageId; order: EventOrder }>
  | Readonly<{ type: "tool"; toolCallId: ToolCallId; order: EventOrder }>
  | Readonly<{ type: "bash"; messageId: MessageId; order: EventOrder }>
  | Readonly<{ type: "extension"; extensionEventId: string; order: EventOrder }>;
