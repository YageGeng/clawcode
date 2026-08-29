import type {
  EntryId,
  EventMeta,
  MessageId,
  QueueId,
  RunId,
  SessionId,
  TimestampMs,
  ToolCallId,
  TerminalId,
  TurnId
} from "../acp/protocol";
import type { ImageContentBlock, ImageMimeType } from "../acp/protocol";

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
  status?: "succeeded" | "failed";
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
  sequenceRange?: EventSequenceRange;
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

export type TerminalStatus = "running" | "exited" | "failed";

export type TerminalSnapshot = Readonly<{
  terminalId: TerminalId;
  command: string;
  cwd: string;
  tty: boolean;
  status: TerminalStatus;
  exitCode?: number | null;
  startedAt: TimestampMs;
  lastActivityAt: TimestampMs;
}>;

export type TerminalListResult = Readonly<{
  revision: number;
  terminals: readonly TerminalSnapshot[];
}>;

export type TerminalUpdateNotification = Readonly<{
  sessionId: SessionId;
  revision: number;
}>;

/** Strictly decodes terminal extension payloads at the ACP trust boundary. */
export const TerminalProtocol = {
  decodeList(value: unknown): TerminalListResult {
    const record = terminalRecord(value, "terminal list");
    terminalFields(record, ["revision", "terminals"], "terminal list");
    const terminals = record.terminals;
    if (!Array.isArray(terminals)) throw new Error("terminal list terminals must be an array");
    return {
      revision: terminalRevision(record.revision, "terminal list revision"),
      terminals: terminals.map((terminal, index) => terminalSnapshot(terminal, index))
    };
  },

  decodeUpdate(value: unknown): TerminalUpdateNotification {
    const record = terminalRecord(value, "terminal update");
    terminalFields(record, ["sessionId", "revision"], "terminal update");
    if (typeof record.sessionId !== "string" || record.sessionId.length === 0) {
      throw new Error("terminal update sessionId must be a non-empty string");
    }
    return {
      sessionId: record.sessionId as SessionId,
      revision: terminalRevision(record.revision, "terminal update revision")
    };
  }
} as const;

/** Narrows one terminal payload to a JSON object. */
function terminalRecord(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

/** Rejects protocol fields that are not part of one terminal payload shape. */
function terminalFields(record: Record<string, unknown>, allowed: readonly string[], label: string): void {
  const unknown = Object.keys(record).find((key) => !allowed.includes(key));
  if (unknown !== undefined) throw new Error(`${label} contains unknown field ${unknown}`);
}

/** Decodes one non-negative JavaScript-safe terminal revision. */
function terminalRevision(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new Error(`${label} must be a non-negative safe integer`);
  }
  return value as number;
}

/** Decodes one complete retained-terminal snapshot without unchecked casts. */
function terminalSnapshot(value: unknown, index: number): TerminalSnapshot {
  const record = terminalRecord(value, `terminal snapshot ${index}`);
  terminalFields(
    record,
    ["terminalId", "command", "cwd", "tty", "status", "exitCode", "startedAt", "lastActivityAt"],
    `terminal snapshot ${index}`
  );
  const terminalId = record.terminalId;
  const status = record.status;
  const exitCode = record.exitCode;
  if (!Number.isSafeInteger(terminalId) || (terminalId as number) < 1_000 || (terminalId as number) > 99_999) {
    throw new Error(`terminal snapshot ${index} terminalId is outside 1000..99999`);
  }
  if (typeof record.command !== "string" || typeof record.cwd !== "string" || typeof record.tty !== "boolean") {
    throw new Error(`terminal snapshot ${index} has invalid command, cwd, or tty`);
  }
  if (status !== "running" && status !== "exited" && status !== "failed") {
    throw new Error(`terminal snapshot ${index} has invalid status`);
  }
  if (
    exitCode !== undefined
    && exitCode !== null
    && (!Number.isInteger(exitCode) || (exitCode as number) < -2_147_483_648 || (exitCode as number) > 2_147_483_647)
  ) {
    throw new Error(`terminal snapshot ${index} exitCode must be an integer or null`);
  }
  if (
    typeof record.startedAt !== "string"
    || !/^\d+$/.test(record.startedAt)
    || typeof record.lastActivityAt !== "string"
    || !/^\d+$/.test(record.lastActivityAt)
  ) {
    throw new Error(`terminal snapshot ${index} timestamps must be strings`);
  }
  return {
    terminalId: terminalId as TerminalId,
    command: record.command,
    cwd: record.cwd,
    tty: record.tty,
    status,
    ...(exitCode === undefined ? {} : { exitCode: exitCode as number | null }),
    startedAt: record.startedAt as TimestampMs,
    lastActivityAt: record.lastActivityAt as TimestampMs
  };
}

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
  | Readonly<{ type: "extension"; extensionEventId: string; order: EventOrder }>
  | Readonly<{ type: "compaction"; entryId: EntryId; order: EventOrder }>
  | Readonly<{ type: "agent_usage"; runId: RunId; order: EventOrder }>
  | Readonly<{ type: "recovery"; notice: RecoveryNotice; order: EventOrder }>;
