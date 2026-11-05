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
  inputTokens?: string;
  outputTokens?: string;
  cacheReadTokens?: string;
  cacheWriteTokens?: string;
  reasoningTokens?: string;
  totalTokens?: string;
}>;

export type AgentRunUsage = Readonly<{
  usage: ModelUsage;
  /** Input and cache reads summed per `provider/model`, the basis of cache hit rates. */
  cacheByModel: ReadonlyMap<string, ModelUsage>;
  settled: boolean;
}>;

export const ModelUsages = {
  /** Formats reported counts consistently, keeping unknown values visible. */
  formatTokens(value: string | undefined): string {
    if (value === undefined) return "未知";
    try {
      return BigInt(value).toLocaleString();
    } catch {
      // Preserve malformed diagnostics instead of crashing the transcript.
      return value;
    }
  },

  /**
   * Formats the share of input tokens read from the provider cache, or `undefined` when the
   * provider did not report both counters. Upstream input already includes cache reads.
   *
   * Over a sum, a request without a cache-read count counts as a miss: Gemini and legacy
   * records omit zero reads, so dropping them would overstate the rate. Mixed-model sums
   * go through `AgentRunUsages.cacheHitRate`, which leaves out models that never report.
   */
  cacheHitRate(usage: ModelUsage): string | undefined {
    if (usage.inputTokens === undefined || usage.cacheReadTokens === undefined) return undefined;
    const input = BigInt(usage.inputTokens);
    if (input === 0n) return undefined;
    // Per-mille in BigInt keeps one decimal place without losing u64 precision;
    // a provider reporting reads above input still never shows more than 100%.
    const perMille = (BigInt(usage.cacheReadTokens) * 1000n) / input;
    return `${(Number(perMille > 1000n ? 1000n : perMille) / 10).toFixed(1)}%`;
  },

  /** Mirrors upstream context_tokens without inventing a reported billing total. */
  contextTokens(usage: ModelUsage): string | undefined {
    if (usage.totalTokens !== undefined) return usage.totalTokens;
    if (usage.inputTokens === undefined) return undefined;
    const tokens = BigInt(usage.inputTokens) + BigInt(usage.outputTokens ?? "0");
    // Upstream uses u64 saturating addition; Number would also lose precision.
    return (tokens > 18446744073709551615n ? 18446744073709551615n : tokens).toString();
  },

  /** Sums only reported counters, preserving unknown values and integer precision. */
  sum(usages: readonly ModelUsage[]): ModelUsage {
    const fields = ["inputTokens", "outputTokens", "cacheReadTokens", "cacheWriteTokens", "reasoningTokens", "totalTokens"] as const;
    const totals: Partial<Record<keyof ModelUsage, bigint>> = {};
    for (const usage of usages) {
      for (const field of fields) {
        const value = usage[field];
        if (value !== undefined) totals[field] = (totals[field] ?? 0n) + BigInt(value);
      }
    }
    const result: Partial<Record<keyof ModelUsage, string>> = {};
    for (const field of fields) {
      const value = totals[field];
      if (value !== undefined) result[field] = value.toString();
    }
    return result;
  }
} as const;

export const AgentRunUsages = {
  /** Keys cache usage by the provider and model that served a request. */
  modelKey(assistant: AssistantDiagnostics): string {
    return `${assistant.providerId}/${assistant.modelId}`;
  },

  /** Adds one settled Turn's Assistant usage to its Run, starting the Run when absent. */
  settleTurn(previous: AgentRunUsage | undefined, assistants: readonly AssistantDiagnostics[]): AgentRunUsage {
    const cacheByModel = new Map(previous?.cacheByModel);
    for (const assistant of assistants) {
      const key = AgentRunUsages.modelKey(assistant);
      const { inputTokens, cacheReadTokens } = assistant.usage;
      const counted = {
        ...(inputTokens === undefined ? {} : { inputTokens }),
        ...(cacheReadTokens === undefined ? {} : { cacheReadTokens })
      };
      const before = cacheByModel.get(key);
      cacheByModel.set(key, before === undefined ? ModelUsages.sum([counted]) : ModelUsages.sum([before, counted]));
    }
    const usages = assistants.map((assistant) => assistant.usage);
    return {
      usage: ModelUsages.sum(previous === undefined ? usages : [previous.usage, ...usages]),
      cacheByModel,
      settled: previous?.settled ?? false
    };
  },

  /**
   * Cache hit rate over the models that reported cache reads at least once. A model that
   * never reports them, such as a gateway without cache data, is left out instead of
   * lowering the rate; within a reporting model a missing count is a miss.
   */
  cacheHitRate(runs: Iterable<AgentRunUsage>): string | undefined {
    const byModel = new Map<string, ModelUsage>();
    for (const run of runs) {
      for (const [key, usage] of run.cacheByModel) {
        const before = byModel.get(key);
        byModel.set(key, before === undefined ? usage : ModelUsages.sum([before, usage]));
      }
    }
    const reporting = [...byModel.values()].filter((usage) => usage.cacheReadTokens !== undefined);
    return reporting.length === 0 ? undefined : ModelUsages.cacheHitRate(ModelUsages.sum(reporting));
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

/** A significant prompt-cache miss on one Assistant request and its likely causes. */
export type CacheMiss = Readonly<{
  /** Tokens of the previous request's prompt that this request did not read from cache. */
  missedTokens: string;
  /** Milliseconds between the previous request's end and this request's start. */
  idleMs: number;
  /** Whether the provider or model changed since the previous request. */
  modelChanged: boolean;
}>;

/** The latest Assistant request whose prompt the next request should read from cache. */
type CacheRequest = Readonly<{ modelKey: string; inputTokens: bigint; endedAtMs: number }>;

/**
 * Per-Session cache-miss state. It advances once per settled Turn, so replay stays linear,
 * and keeps misses by message so a re-decoded message does not drop its diagnosis.
 */
export type CacheTrail = Readonly<{
  /** Models that have read from cache at least once in this Session. */
  caching: ReadonlySet<string>;
  last: CacheRequest | undefined;
  misses: ReadonlyMap<MessageId, CacheMiss>;
}>;

export const CacheMisses = {
  /** Misses at or below this many tokens are cache-breakpoint granularity noise. */
  noiseFloor: 1024n,
  /** Gaps longer than the common five-minute cache lifetime likely expired the entry. */
  ttlMs: 300_000,
  empty: { caching: new Set(), last: undefined, misses: new Map() } as CacheTrail,

  /**
   * Advances the trail over one settled Turn's messages, recording each Assistant request
   * that missed a significant part of the previous request's prompt. A request is judged
   * only when its model has read from cache (with caching off every read is zero, which is
   * not a miss) and reports its own reads, since an omitted count cannot be told apart from
   * zero. A shrunken prompt (compaction or branch navigation) is a rewrite, not a miss.
   */
  settleTurn(trail: CacheTrail, messages: readonly MessageEntity[]): CacheTrail {
    const requests = messages
      .flatMap((message) => {
        const assistant = message.assistant;
        const input = assistant?.usage.inputTokens;
        return assistant === undefined || input === undefined ? [] : [{ message, assistant, input: BigInt(input) }];
      })
      .sort((left, right) => Number(left.message.startedAtMs) - Number(right.message.startedAtMs));
    if (requests.length === 0) return trail;
    const caching = new Set(trail.caching);
    for (const { assistant } of requests) {
      if (BigInt(assistant.usage.cacheReadTokens ?? "0") > 0n) caching.add(AgentRunUsages.modelKey(assistant));
    }
    const found = new Map<MessageId, CacheMiss>();
    let last = trail.last;
    for (const { message, assistant, input } of requests) {
      const modelKey = AgentRunUsages.modelKey(assistant);
      const cacheRead = assistant.usage.cacheReadTokens;
      const missed = last === undefined || cacheRead === undefined ? 0n : last.inputTokens - BigInt(cacheRead);
      if (last !== undefined && caching.has(modelKey) && input >= last.inputTokens && missed > CacheMisses.noiseFloor) {
        found.set(message.messageId, {
          missedTokens: missed.toString(),
          idleMs: Number(message.startedAtMs) - last.endedAtMs,
          modelChanged: last.modelKey !== modelKey
        });
      }
      last = { modelKey, inputTokens: input, endedAtMs: Number(message.endedAtMs) };
    }
    const misses = found.size === 0 ? trail.misses : new Map([...trail.misses, ...found]);
    return { caching, last, misses };
  },

  /** Describes a miss with its likely causes for message diagnostics. */
  describe(miss: CacheMiss): string {
    const causes = [
      ...(miss.idleMs > CacheMisses.ttlMs ? [`idle ${Math.round(miss.idleMs / 60_000)}m`] : []),
      ...(miss.modelChanged ? ["model changed"] : [])
    ];
    return [`${BigInt(miss.missedTokens).toLocaleString()} tokens`, ...causes].join(" · ");
  }
} as const;

export type ContextUsage = Readonly<{
  used?: string;
  size: string;
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
  submissionId?: string;
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
  /** Absent for a request-scoped elicitation such as a sign-in, which has no Session. */
  sessionId?: SessionId;
  serverId?: string;
  message: string;
  mode: Readonly<{ type: "form"; requestedSchema: unknown }> | Readonly<{ type: "url"; url: string }>;
  /** Present when this elicitation carries a sign-in device code. */
  login?: Readonly<{ userCode: string; name: string }>;
}>;

/** One todo item as the agent's plan update shows it. */
export type PlanTodo = Readonly<{ content: string; status: string }>;

/** The plans the agent published for one Session, keyed by their stable plan ids. */
export type SessionPlans = Readonly<{
  /** The `todos` plan: the agent's current todo list. */
  todos?: readonly PlanTodo[];
  /** The `plan-mode` plan: Markdown of the plan the user last approved. */
  approvedPlan?: string;
}>;

/** One option the agent offers in a `session/request_permission`. */
export type PermissionOption = Readonly<{ optionId: string; name: string; kind: string }>;

/** One standard `session/request_permission` waiting for the user, oldest first. */
export type PermissionPrompt = Readonly<{
  key: string;
  sessionId: SessionId;
  title: string;
  description?: string;
  /** Present when the request approves a plan submitted with `exit_plan_mode`. */
  plan?: string;
  options: readonly PermissionOption[];
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

/**
 * Stable transcript row keys shared by row rendering and Tree-driven reveal so
 * one durable Tree entry can find the DOM row that projects it.
 */
export const TranscriptRowKeys = {
  message: (messageId: MessageId) => `message:${messageId}`,
  tool: (toolCallId: ToolCallId) => `tool:${toolCallId}`,
  extension: (extensionEventId: string) => `extension:${extensionEventId}`,
  compaction: (entryId: EntryId) => `compaction:${entryId}`,

  /** Returns the key one row renders under; usage and recovery rows have no Tree node. */
  of(row: TranscriptEntry): string | undefined {
    switch (row.type) {
      case "message": return TranscriptRowKeys.message(row.messageId);
      case "tool": return TranscriptRowKeys.tool(row.toolCallId);
      case "extension": return TranscriptRowKeys.extension(row.extensionEventId);
      case "compaction": return TranscriptRowKeys.compaction(row.entryId);
      default: return undefined;
    }
  }
} as const;
