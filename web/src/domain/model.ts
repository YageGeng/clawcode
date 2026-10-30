import type {
  EntryId,
  EventMeta,
  MessageId,
  QueueId,
  RunId,
  SessionId,
  TimestampMs,
  ToolCallId,
  TurnId
} from "../acp/protocol";
import type { ImageContentBlock, ImageMimeType } from "../acp/protocol";

export type SessionOwnership = Readonly<
  { kind: "tool"; sessionId: SessionId; toolCallId: string }
  | { kind: "background"; sessionId: SessionId; name: string }
>;

export type SessionSummary = Readonly<{
  sessionId: SessionId;
  cwd: string;
  title: string;
  createdAtMs?: TimestampMs;
  modifiedAtMs?: TimestampMs;
  parentSessionId?: SessionId;
  ownership?: SessionOwnership;
}>;

export type EventOrder = Readonly<{
  sequence: number;
  receivedOrder: number;
}>;

export type EventSequenceRange = Readonly<{
  start: number;
  end: number;
}>;

export type RecoveryNotice = Readonly<{
  recoveryId: string;
  eventCount: number;
  sequenceRange: EventSequenceRange;
}>;

export type RecoveryCommit = Readonly<{
  notice: RecoveryNotice;
  order: EventOrder;
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

export type AgentRunUsage = Readonly<{
  usage: ModelUsage;
  settled: boolean;
}>;

export const ModelUsages = {
  /** Aggregates exact per-attempt token counters without losing integer precision. */
  sum(usages: readonly ModelUsage[]): ModelUsage {
    let inputTokens = 0n;
    let outputTokens = 0n;
    let cacheReadTokens = 0n;
    let cacheWriteTokens = 0n;
    let reasoningTokens = 0n;
    let hasReasoningTokens = false;
    let totalTokens = 0n;
    for (const usage of usages) {
      inputTokens += BigInt(usage.inputTokens);
      outputTokens += BigInt(usage.outputTokens);
      cacheReadTokens += BigInt(usage.cacheReadTokens);
      cacheWriteTokens += BigInt(usage.cacheWriteTokens);
      totalTokens += BigInt(usage.totalTokens);
      if (usage.reasoningTokens !== undefined) {
        reasoningTokens += BigInt(usage.reasoningTokens);
        hasReasoningTokens = true;
      }
    }
    return {
      inputTokens: inputTokens.toString(),
      outputTokens: outputTokens.toString(),
      cacheReadTokens: cacheReadTokens.toString(),
      cacheWriteTokens: cacheWriteTokens.toString(),
      ...(hasReasoningTokens ? { reasoningTokens: reasoningTokens.toString() } : {}),
      totalTokens: totalTokens.toString()
    };
  }
} as const;

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
  | Readonly<{ type: "finished"; reason: CompactionReason; entryId: EntryId; endedAtMs: TimestampMs }>
  | Readonly<{ type: "failed"; reason: CompactionReason; message: string; endedAtMs: TimestampMs }>
  | Readonly<{ type: "cancelled"; reason: CompactionReason; endedAtMs: TimestampMs }>;

export type CompactionEntity = Readonly<{
  entryId: EntryId;
  turnId: TurnId;
  reason: CompactionReason;
  summary: string;
  tokensBefore: string;
  usage?: ModelUsage;
  startedAtMs: TimestampMs;
  endedAtMs: TimestampMs;
}>;

export type MessageEntity = Readonly<{
  messageId: MessageId;
  turnId: TurnId;
  role: "user" | "assistant" | "system";
  text: string;
  images: readonly ImageContentBlock[];
  reasoning: string;
  timestampMs: TimestampMs;
  startedAtMs: TimestampMs;
  endedAtMs: TimestampMs;
  streaming: boolean;
  sequenceRange?: EventSequenceRange;
  assistant?: AssistantDiagnostics;
  slashCommand?: SlashCommandMessageMeta;
}>;

export type SlashCommandSource = "builtin" | "extension" | "skill" | "prompt_template";

export type SlashCommandAliasKind = "canonical" | "short";

export type SlashCommandMessageMeta = Readonly<{
  name: string;
  source: SlashCommandSource;
  messageKind: "invocation" | "output" | "expansion";
  status?: "succeeded" | "failed" | "cancelled";
}>;

export type ToolCallEntity = Readonly<{
  toolCallId: ToolCallId;
  title: string;
  status: "pending" | "in_progress" | "completed" | "failed" | "blocked" | "cancelled";
  kind?: ToolKind;
  /** Set for a user's own `!` command, which is shown as a tool. */
  origin?: "user";
  excludeFromContext?: boolean;
  rawInput?: unknown;
  rawOutput?: unknown;
  subagentSessionId?: SessionId;
  output?: ToolOutputInfo;
  content: readonly unknown[];
  startedAtMs?: TimestampMs;
  endedAtMs?: TimestampMs;
  sequenceRange?: EventSequenceRange;
  meta?: EventMeta;
}>;

/** ACP tool kinds the agent reports; drives the tool card icon. */
export type ToolKind = "read" | "edit" | "delete" | "move" | "search" | "execute" | "think" | "fetch" | "switch_mode" | "other";

/** One live-only Session warning shown as a dismissible toast. */
export type SessionNotice = Readonly<{
  id: string;
  sessionId: SessionId;
  title: string;
  description?: string;
}>;

export type ToolOutputInfo = Readonly<{
  originalTextBytes: number;
  visibleTextBytes: number;
  omittedMedia: number;
  fullOutputPath?: string;
  saveFailure?: "io" | "too_large" | "cancelled";
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

export type PromptImage = Readonly<{
  id: string;
  name: string;
  size: number;
  data: string;
  mimeType: ImageMimeType;
}>;

export type PromptInput = Readonly<{
  text: string;
  resources: readonly PromptResourceLink[];
  images: readonly PromptImage[];
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

export type SessionRuntimeSnapshot = Readonly<{
  sessionId: SessionId;
  running: boolean;
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
  source?: SlashCommandSource;
  qualifiedName?: string;
  aliasKind?: SlashCommandAliasKind;
}>;

export type McpToolInfo = Readonly<{
  reference: Readonly<{ serverId: string; remoteName: string }>;
  publicName: string;
  description?: string;
  inputSchema: unknown;
  title?: string;
  loaded?: boolean;
  disabled?: boolean;
  metadata?: Readonly<{
    exposure: "direct" | "deferred" | "hidden";
    annotations: Readonly<{ readOnlyHint?: boolean; destructiveHint?: boolean; idempotentHint?: boolean; openWorldHint?: boolean }>;
    outputSchema?: unknown;
  }>;
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

/** One standard `elicitation/create` waiting for the user, oldest first in the queue. */
export type ElicitationPrompt = Readonly<{
  key: string;
  sessionId: SessionId;
  serverId?: string;
  message: string;
  mode: Readonly<{ type: "form"; requestedSchema: unknown }> | Readonly<{ type: "url"; url: string }>;
}>;

/** The user's answer, serialized as the standard elicitation response. */
export type ElicitationAnswer =
  | Readonly<{ action: "accept"; content?: Readonly<Record<string, unknown>> }>
  | Readonly<{ action: "decline" }>
  | Readonly<{ action: "cancel" }>;

export type McpServerStatus = Readonly<{
  serverId: string;
  configuredPolicy: "auto" | "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05";
  negotiatedProtocol?: "2025-11-25" | "2025-06-18" | "2025-03-26" | "2024-11-05";
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
  /** Terminal whose whole output this event carries; a newer one replaces it in the log. */
  snapshotOf?: string;
}>;

export type TranscriptEntry =
  | Readonly<{ type: "message"; messageId: MessageId; order: EventOrder }>
  | Readonly<{ type: "tool"; toolCallId: ToolCallId; order: EventOrder }>
  | Readonly<{ type: "extension"; extensionEventId: string; order: EventOrder }>
  | Readonly<{ type: "compaction"; entryId: EntryId; order: EventOrder }>
  | Readonly<{ type: "agent_usage"; runId: RunId; order: EventOrder }>
  | Readonly<{ type: "recovery"; notice: RecoveryNotice; order: EventOrder }>;
