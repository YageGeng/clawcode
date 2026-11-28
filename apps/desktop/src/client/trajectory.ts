// The trajectory of a Session as `_clawcode/session/trajectory` pages it: decoding, the loaded log with its paging and
// catch-up, the rows the record table shows, and the timeline overview's geometry. Nothing here draws.
import { signal } from "nsh";

import { AcpProtocol } from "./acp/protocol";

/** Token counts of one request or a sum of them; an unreported counter counts as zero. */
export type TokenUsage = Readonly<{ input: number; output: number; cached: number; reasoning: number }>;

/** Generation options a request was sent with. */
export type RequestOptions = Readonly<{
  providerId: string; modelId: string; thinkingLevel: string; maxTokens?: number; temperature?: number; promptCache?: string;
}>;

/** What one record is; the shapes follow `TrajectoryRecordKind` of the backend protocol. */
export type RecordKind =
  | Readonly<{ type: "user"; text: string; imageCount: number; fileCount: number; origin: string }>
  | Readonly<{ type: "command"; command: string; text: string }>
  | Readonly<{
    type: "request"; index: number; systemPrompt: string; tools: string; options: RequestOptions; messageCount: number;
    first: boolean; promptChanged: boolean; toolsAdded: readonly string[]; toolsRemoved: readonly string[];
  }>
  | Readonly<{
    type: "assistant"; requestIndex: number; providerId: string; modelId: string; reasoning: string; text: string;
    toolCallIds: readonly string[]; usage: TokenUsage; requestedAtMs: number; firstTokenAtMs: number; lastTokenAtMs: number;
    stopReason: string; error?: string; cancelled: boolean;
  }>
  | Readonly<{
    type: "tool"; callId: string; name: string; arguments: unknown; resultText?: string; imageCount: number; isError: boolean;
    cancelled: boolean;
  }>
  | Readonly<{ type: "compaction"; summary: string; tokensBefore?: number }>
  | Readonly<{ type: "branch_summary"; summary: string }>
  | Readonly<{ type: "setting"; change: string; detail: string }>
  | Readonly<{ type: "extension"; extensionId: string; customType: string; text: string }>;

/** One row of a Session's trajectory; `raw` is the record as the host sent it, for the details' raw view. */
export type TrajectoryRecord = Readonly<{
  id: string;
  sequence: number;
  /** The Run (agent loop) the record belongs to, numbered from 1 on the branch; absent between Runs. */
  run?: Readonly<{ id: string; index: number }>;
  startedAtMs?: number;
  endedAtMs?: number;
  kind: RecordKind;
  raw: unknown;
}>;

/** One decoded page. */
export type TrajectoryPage = Readonly<{
  records: readonly TrajectoryRecord[];
  hasMoreBefore: boolean;
  hasMoreAfter: boolean;
  firstSequence?: number;
  lastSequence?: number;
  usageBefore: TokenUsage;
  branchChanged: boolean;
}>;

/** No tokens. */
export const NO_USAGE: TokenUsage = { input: 0, output: 0, cached: 0, reasoning: 0 };

/** Adds two usages counter by counter. */
export function addUsage(left: TokenUsage, right: TokenUsage): TokenUsage {
  return { input: left.input + right.input, output: left.output + right.output, cached: left.cached + right.cached,
    reasoning: left.reasoning + right.reasoning };
}

/** A non-negative count, or zero. */
function count(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : 0;
}

/** A timestamp string or number as milliseconds; `undefined` when absent or malformed. */
function time(value: unknown): number | undefined {
  const parsed = typeof value === "string" ? Number(value) : value;
  return typeof parsed === "number" && Number.isFinite(parsed) ? parsed : undefined;
}

/** A string field, or `fallback`. */
function text(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

/** A list of strings, others left out. */
function strings(value: unknown): readonly string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

/** The `rig-core-v1` usage record (`{ format, usage: { input_tokens, … } }`) as counts. */
export function decodeUsage(value: unknown): TokenUsage {
  const usage = AcpProtocol.record(AcpProtocol.record(value)?.usage) ?? {};
  return { input: count(usage.input_tokens), output: count(usage.output_tokens), cached: count(usage.cached_input_tokens),
    reasoning: count(usage.reasoning_tokens) };
}

/** Decodes a record's `kind`; `undefined` for a kind this client does not know. */
function decodeKind(value: Record<string, unknown>): RecordKind | undefined {
  switch (value.type) {
    case "user":
      return { type: "user", text: text(value.text), imageCount: count(value.imageCount), fileCount: count(value.fileCount),
        origin: text(value.origin, "input") };
    case "command":
      return { type: "command", command: text(value.kind, "slash"), text: text(value.text) };
    case "request": {
      const snapshot = AcpProtocol.record(value.snapshot) ?? {};
      const options = AcpProtocol.record(snapshot.options) ?? {};
      return {
        type: "request", index: count(value.index), systemPrompt: text(snapshot.systemPrompt), tools: text(snapshot.tools),
        options: {
          providerId: text(options.providerId), modelId: text(options.modelId), thinkingLevel: text(options.thinkingLevel),
          ...(typeof options.maxTokens === "number" ? { maxTokens: options.maxTokens } : {}),
          ...(typeof options.temperature === "number" ? { temperature: options.temperature } : {}),
          ...(typeof options.promptCache === "string" ? { promptCache: options.promptCache } : {}),
        },
        messageCount: count(snapshot.messageCount), first: value.first === true, promptChanged: value.promptChanged === true,
        toolsAdded: strings(value.toolsAdded), toolsRemoved: strings(value.toolsRemoved),
      };
    }
    case "assistant":
      return {
        type: "assistant", requestIndex: count(value.requestIndex), providerId: text(value.providerId), modelId: text(value.modelId),
        reasoning: text(value.reasoning), text: text(value.text), toolCallIds: strings(value.toolCallIds), usage: decodeUsage(value.usage),
        requestedAtMs: time(value.requestedAtMs) ?? 0, firstTokenAtMs: time(value.firstTokenAtMs) ?? 0,
        lastTokenAtMs: time(value.lastTokenAtMs) ?? 0, stopReason: text(value.stopReason),
        ...(typeof value.error === "string" ? { error: value.error } : {}), cancelled: value.cancelled === true,
      };
    case "tool":
      return {
        type: "tool", callId: text(value.callId), name: text(value.name), arguments: value.arguments,
        ...(typeof value.resultText === "string" ? { resultText: value.resultText } : {}), imageCount: count(value.imageCount),
        isError: value.isError === true, cancelled: value.cancelled === true,
      };
    case "compaction":
      return { type: "compaction", summary: text(value.summary),
        ...(typeof value.tokensBefore === "number" ? { tokensBefore: value.tokensBefore } : {}) };
    case "branch_summary":
      return { type: "branch_summary", summary: text(value.summary) };
    case "setting": {
      // `model` names a provider and model, `thinking_level` a level, `active_tools` a tool list.
      const detail = value.change === "model" ? `${text(value.provider)}/${text(value.model)}`
        : value.change === "thinking_level" ? text(value.level) : strings(value.tools).join(", ");
      return { type: "setting", change: text(value.change), detail };
    }
    case "extension":
      return { type: "extension", extensionId: text(value.extensionId), customType: text(value.customType), text: text(value.text) };
    default:
      return undefined;
  }
}

/** Decodes a `_clawcode/session/trajectory` reply; throws for one that is not a page. Unknown record kinds are left out. */
export function decodeTrajectoryPage(reply: unknown): TrajectoryPage {
  const page = AcpProtocol.decodeRecord(reply, "Trajectory page");
  if (!Array.isArray(page.records)) throw new Error("Trajectory page has no records");
  const records = page.records.flatMap((value: unknown): TrajectoryRecord[] => {
    const record = AcpProtocol.record(value);
    const kind = AcpProtocol.record(record?.kind);
    if (typeof record?.id !== "string" || typeof record.sequence !== "number" || kind === undefined) return [];
    const decoded = decodeKind(kind);
    if (decoded === undefined) return [];
    const run = AcpProtocol.record(record.run);
    const startedAtMs = time(record.startedAtMs);
    const endedAtMs = time(record.endedAtMs);
    return [{
      id: record.id, sequence: record.sequence,
      ...(typeof run?.id === "string" && typeof run.index === "number" ? { run: { id: run.id, index: run.index } } : {}),
      ...(startedAtMs === undefined ? {} : { startedAtMs }), ...(endedAtMs === undefined ? {} : { endedAtMs }),
      kind: decoded, raw: value,
    }];
  });
  return {
    records, hasMoreBefore: page.hasMoreBefore === true, hasMoreAfter: page.hasMoreAfter === true,
    ...(typeof page.firstSequence === "number" ? { firstSequence: page.firstSequence } : {}),
    ...(typeof page.lastSequence === "number" ? { lastSequence: page.lastSequence } : {}),
    usageBefore: decodeUsage(page.usageBefore), branchChanged: page.branchChanged === true,
  };
}

/** The query of one page: the tail, the page before a sequence, or the records after one. */
export type TrajectoryQuery = Readonly<{ before?: number; after?: number }>;

/** Why the log could not load, for the view to word: the host keeps no history database, or the request failed. */
export type TrajectoryFailure = Readonly<{ kind: "unavailable" | "failed"; detail: string }>;

/**
 * The loaded part of one Session's trajectory, newest page first, with older pages prepended on request and newer
 * records caught up after the Session changes. Its signals are what the trajectory view and record details read.
 */
export class TrajectoryLog {
  /** Records in branch order. */
  readonly records = signal<readonly TrajectoryRecord[]>([]);
  /** Older records exist before the first loaded one. */
  readonly hasMoreBefore = signal(false);
  /** The first load has not answered yet. */
  readonly loading = signal(true);
  /** An older page is loading. */
  readonly loadingEarlier = signal(false);
  /** The first load failed: nothing to show. */
  readonly failure = signal<TrajectoryFailure | undefined>(undefined);
  /** A later catch-up failed: what is loaded stays. */
  readonly refreshFailure = signal<TrajectoryFailure | undefined>(undefined);
  /** Usage of the branch's requests before the first loaded record. */
  readonly usageBefore = signal<TokenUsage>(NO_USAGE);
  private firstSequence?: number;
  private lastSequence?: number;
  private refreshing?: Promise<void>;
  private refreshAgain = false;
  private loadingTail?: Promise<void>;
  // Bumped by every `replace`: a page asked for before it belongs to records that are gone, so it is dropped.
  private generation = 0;

  /** A log over `fetch`, which answers one page query (and rejects with the host's error). */
  constructor(private readonly fetch: (query: TrajectoryQuery) => Promise<unknown>, private readonly classify: (error: unknown) => TrajectoryFailure) {}

  /**
   * Whether `record` is a tool call still waiting for its result: one of the latest loaded Run. An older call without
   * a result (its Run was interrupted, say by a crash) will never get one, so it neither reads as running nor makes
   * every catch-up page through history again.
   */
  pending(record: TrajectoryRecord): boolean {
    const latest = this.records().at(-1)?.run?.id;
    return record.kind.type === "tool" && record.kind.resultText === undefined && !record.kind.cancelled
      && latest !== undefined && record.run?.id === latest;
  }

  /** The loaded record with id `recordId`. */
  record(recordId: string | undefined): TrajectoryRecord | undefined {
    return recordId === undefined ? undefined : this.records().find((record) => record.id === recordId);
  }

  /** Loads the newest page, replacing whatever was loaded; calls while one runs share it. */
  load(): Promise<void> {
    this.loadingTail ??= (async () => {
      this.loading.set(true);
      this.failure.set(undefined);
      try {
        this.replace(decodeTrajectoryPage(await this.fetch({})));
      } catch (error: unknown) {
        this.failure.set(this.classify(error));
      } finally {
        this.loading.set(false);
        this.loadingTail = undefined;
      }
    })();
    return this.loadingTail;
  }

  /** Prepends the page before the first loaded record; a failure is reported as a refresh failure. */
  async loadEarlier(): Promise<void> {
    if (this.loadingEarlier() || !this.hasMoreBefore() || this.firstSequence === undefined) return;
    this.loadingEarlier.set(true);
    const generation = this.generation;
    try {
      const page = decodeTrajectoryPage(await this.fetch({ before: this.firstSequence }));
      if (generation !== this.generation) return;
      const known = new Set(this.records().map((record) => record.id));
      this.records.set([...page.records.filter((record) => !known.has(record.id)), ...this.records()]);
      this.hasMoreBefore.set(page.hasMoreBefore);
      this.usageBefore.set(page.usageBefore);
      if (page.firstSequence !== undefined) this.firstSequence = page.firstSequence;
    } catch (error: unknown) {
      this.refreshFailure.set(this.classify(error));
    } finally {
      this.loadingEarlier.set(false);
    }
  }

  /** Keeps loading older pages until the branch's start is loaded. */
  async loadAll(): Promise<void> {
    while (this.hasMoreBefore() && !this.refreshFailure()) {
      const before = this.firstSequence;
      await this.loadEarlier();
      if (this.firstSequence === before) return;
    }
  }

  /**
   * Catches up with records after the loaded ones. A tool record still waiting for its result is asked for again, so
   * the query starts just before the earliest one; everything loaded from there on is replaced by the answer. Calls
   * while one runs are merged into a single follow-up.
   */
  refresh(): Promise<void> {
    if (this.refreshing) {
      this.refreshAgain = true;
      return this.refreshing;
    }
    this.refreshing = (async () => {
      try {
        do {
          this.refreshAgain = false;
          await this.catchUp();
        } while (this.refreshAgain);
      } finally {
        this.refreshing = undefined;
      }
    })();
    return this.refreshing;
  }

  /** One catch-up: pages `after` the anchor until the host has nothing newer. */
  private async catchUp(): Promise<void> {
    if (this.lastSequence === undefined) return this.load();
    let generation = this.generation;
    try {
      const pending = this.records().find((record) => this.pending(record));
      // An `after` cursor must name an entry on the branch: the host reads any other number as a branch change and
      // answers with the tail. Sequences also number entries that are not on it, so step back to the last loaded record
      // before the pending one rather than to the number before it.
      const before = pending && this.records().findLast((record) => record.sequence < pending.sequence);
      // With nothing loaded before it, only the tail can bring the pending call's result.
      if (pending && !before) return this.load();
      let anchor = before ? before.sequence : this.lastSequence;
      for (;;) {
        const page = decodeTrajectoryPage(await this.fetch({ after: anchor }));
        // A reload replaced the records meanwhile; its tail is newer than this answer's base.
        if (generation !== this.generation) return;
        if (page.branchChanged) {
          this.replace(page);
          generation = this.generation;
        } else {
          const from = anchor;
          this.records.set([...this.records().filter((record) => record.sequence <= from), ...page.records]);
          if (page.lastSequence !== undefined) this.lastSequence = Math.max(this.lastSequence ?? 0, page.lastSequence);
        }
        this.refreshFailure.set(undefined);
        if (!page.hasMoreAfter || page.lastSequence === undefined) return;
        anchor = page.lastSequence;
      }
    } catch (error: unknown) {
      this.refreshFailure.set(this.classify(error));
    }
  }

  /** Replaces everything loaded with `page`, the tail of the branch. */
  private replace(page: TrajectoryPage): void {
    this.generation += 1;
    this.records.set(page.records);
    this.hasMoreBefore.set(page.hasMoreBefore);
    this.usageBefore.set(page.usageBefore);
    this.firstSequence = page.firstSequence;
    this.lastSequence = page.lastSequence;
    this.refreshFailure.set(undefined);
  }
}

/** One row of the record table. */
export type TrajectoryRow =
  /** A Run's heading: `index` 0 is the stretch between Runs. */
  | Readonly<{ type: "run"; key: string; index: number; durationMs?: number; tokens: number; collapsed: boolean }>
  /** A request's separator, from its request record. */
  | Readonly<{ type: "request"; key: string; record: TrajectoryRecord; firstTokenMs?: number }>
  /** What changed in the request's system prompt or tools; selecting it shows the request. */
  | Readonly<{ type: "system"; key: string; record: TrajectoryRecord }>
  /** Folded tool calls of one request. */
  | Readonly<{ type: "tools"; key: string; count: number; failed: number; durationMs?: number; open: boolean }>
  /** A record; `inGroup` when it is a call listed under an opened tool group. */
  | Readonly<{ type: "record"; key: string; record: TrajectoryRecord; inGroup?: boolean }>;

/** How the table folds: Runs folded by key, tool calls grouped per request unless that group was opened. */
export type RowOptions = Readonly<{
  collapsedRuns: ReadonlySet<string>;
  groupTools: boolean;
  openGroups: ReadonlySet<string>;
  /** Only records overlapping this time range, when set. */
  range?: Readonly<{ start: number; end: number }>;
}>;

/** What a single record says about itself. */
export const TrajectoryRecords = {
  /** How long a record took, when it has both ends. */
  duration(record: TrajectoryRecord): number | undefined {
    return record.startedAtMs !== undefined && record.endedAtMs !== undefined ? Math.max(0, record.endedAtMs - record.startedAtMs) : undefined;
  },

  /** The key of the Run a record belongs to; records between Runs share one key until the next Run. */
  runKey(record: TrajectoryRecord): string {
    return record.run ? `run:${record.run.id}` : "between";
  },
} as const;

/** Whether a record overlaps `range`; a record without times only shows unfiltered. */
export function overlaps(record: TrajectoryRecord, range: Readonly<{ start: number; end: number }>): boolean {
  const start = record.startedAtMs;
  if (start === undefined) return false;
  return start <= range.end && (record.endedAtMs ?? start) >= range.start;
}

/**
 * The key of the heading `record` sits under among `records` (the ones the table shows). A Run's heading is keyed by
 * its Run alone, so loading earlier records of it keeps it folded; a stretch between Runs by its first record.
 */
export function runHeadingKey(records: readonly TrajectoryRecord[], record: TrajectoryRecord): string {
  const key = TrajectoryRecords.runKey(record);
  if (record.run) return key;
  let at = records.indexOf(record);
  while (at > 0 && records[at - 1]!.run === undefined) at--;
  return `${key}:${(records[at] ?? record).id}`;
}

/** The records the table shows: those overlapping the chosen time range, or all. */
export function shownRecords(records: readonly TrajectoryRecord[], range: RowOptions["range"]): readonly TrajectoryRecord[] {
  return range ? records.filter((record) => overlaps(record, range)) : records;
}

/** The record table's rows: a heading per Run, a separator per request, then the records, folded as `options` say. */
export function trajectoryRows(records: readonly TrajectoryRecord[], options: RowOptions): TrajectoryRow[] {
  const rows: TrajectoryRow[] = [];
  const visible = shownRecords(records, options.range);
  // Records come in branch order, so each Run's records are contiguous.
  let index = 0;
  while (index < visible.length) {
    const first = visible[index]!;
    const key = TrajectoryRecords.runKey(first);
    let end = index;
    while (end < visible.length && TrajectoryRecords.runKey(visible[end]!) === key) end++;
    const members = visible.slice(index, end);
    const headingKey = runHeadingKey(visible, first);
    const collapsed = options.collapsedRuns.has(headingKey);
    const starts = members.flatMap((record) => (record.startedAtMs === undefined ? [] : [record.startedAtMs]));
    const ends = members.flatMap((record) => (record.endedAtMs === undefined ? [] : [record.endedAtMs]));
    const tokens = members.reduce((sum, record) => sum + (record.kind.type === "assistant" ? record.kind.usage.input + record.kind.usage.output : 0), 0);
    rows.push({ type: "run", key: headingKey, index: first.run?.index ?? 0, tokens, collapsed,
      ...(starts.length && ends.length ? { durationMs: Math.max(...ends) - Math.min(...starts) } : {}) });
    if (!collapsed) rows.push(...runRows(members, options));
    index = end;
  }
  return rows;
}

/** The rows inside one Run. */
function runRows(records: readonly TrajectoryRecord[], options: RowOptions): TrajectoryRow[] {
  const rows: TrajectoryRow[] = [];
  let tools: TrajectoryRecord[] = [];
  /** Emits the pending tool records, as one folded row when grouping and the group is closed. */
  const flushTools = () => {
    if (!tools.length) return;
    const key = `tools:${tools[0]!.id}`;
    const open = options.openGroups.has(key);
    if (options.groupTools && tools.length > 1) {
      const starts = tools.flatMap((record) => (record.startedAtMs === undefined ? [] : [record.startedAtMs]));
      const ends = tools.flatMap((record) => (record.endedAtMs === undefined ? [] : [record.endedAtMs]));
      rows.push({ type: "tools", key, count: tools.length, open,
        failed: tools.filter((record) => record.kind.type === "tool" && record.kind.isError).length,
        ...(starts.length && ends.length ? { durationMs: Math.max(...ends) - Math.min(...starts) } : {}) });
    }
    if (!options.groupTools || tools.length === 1 || open) {
      // Only calls listed under a group heading are part of a group; a lone call has no heading above it.
      const inGroup = options.groupTools && tools.length > 1;
      for (const record of tools) rows.push({ type: "record", key: record.id, record, ...(inGroup ? { inGroup } : {}) });
    }
    tools = [];
  };
  for (const record of records) {
    if (record.kind.type === "tool") {
      tools.push(record);
      continue;
    }
    flushTools();
    if (record.kind.type === "request") {
      const reply = records.find((other) => other.kind.type === "assistant" && other.kind.requestIndex === (record.kind as { index: number }).index);
      const firstTokenMs = reply?.kind.type === "assistant" && reply.kind.firstTokenAtMs && reply.kind.requestedAtMs
        ? reply.kind.firstTokenAtMs - reply.kind.requestedAtMs : undefined;
      rows.push({ type: "request", key: record.id, record, ...(firstTokenMs === undefined ? {} : { firstTokenMs }) });
      const kind = record.kind;
      if (kind.first || kind.promptChanged || kind.toolsAdded.length || kind.toolsRemoved.length) {
        rows.push({ type: "system", key: `system:${record.id}`, record });
      }
      continue;
    }
    rows.push({ type: "record", key: record.id, record });
  }
  flushTools();
  return rows;
}

/** What the toolbar sums up over the loaded records. */
export type TrajectorySummary = Readonly<{ runs: number; requests: number; tokens: number; cacheHit?: number }>;

/** Counts Runs and requests, sums tokens, and the share of input read from the prompt cache. */
export function summarize(records: readonly TrajectoryRecord[]): TrajectorySummary {
  const runs = new Set<string>();
  let requests = 0;
  let usage = NO_USAGE;
  for (const record of records) {
    if (record.run) runs.add(record.run.id);
    if (record.kind.type === "request") requests++;
    if (record.kind.type === "assistant") usage = addUsage(usage, record.kind.usage);
  }
  return { runs: runs.size, requests, tokens: usage.input + usage.output,
    ...(usage.input > 0 ? { cacheHit: usage.cached / usage.input } : {}) };
}

/** The assistant output of a request record, or the request of an assistant record: one entry recorded both. */
export function pairedRecord(records: readonly TrajectoryRecord[], record: TrajectoryRecord): TrajectoryRecord | undefined {
  const kind = record.kind;
  if (kind.type === "request") return records.find((other) => other.kind.type === "assistant" && other.kind.requestIndex === kind.index);
  if (kind.type === "assistant") return records.find((other) => other.kind.type === "request" && other.kind.index === kind.requestIndex);
  return undefined;
}

/** The request before `record` on the branch, among the loaded ones. */
export function previousRequest(records: readonly TrajectoryRecord[], record: TrajectoryRecord): TrajectoryRecord | undefined {
  const at = records.indexOf(record);
  for (let i = at - 1; i >= 0; i--) if (records[i]!.kind.type === "request") return records[i];
  return undefined;
}

// --- Timeline overview geometry -------------------------------------------------------------------------------------

/** A time window on the overview's axis, in the axis' unit (milliseconds, or record positions when spaced evenly). */
export type Window = Readonly<{ start: number; end: number }>;

/** One bar on a lane: `start` to `end`, a lighter `wait` part until the first token for requests. */
export type TimelineBar = Readonly<{
  id: string; lane: "request" | "tool" | "event"; start: number; end: number; firstToken?: number; failed: boolean;
  /** Bars merged into this one (overflowing tool calls, or bars thinner than a point). */
  count: number;
  /** Sub-row inside the tool lane. */
  row: number;
}>;

/** A Run's start on the axis, for its separator. */
export type RunMark = Readonly<{ index: number; at: number }>;

/** The overview's content in axis units: bars, Run separators, and the full extent. */
export type Timeline = Readonly<{ bars: readonly TimelineBar[]; runs: readonly RunMark[]; extent: Window }>;

/** The most sub-rows the tool lane splits into; later overlapping calls merge into one counted bar. */
export const TOOL_ROWS = 3;

/**
 * Lays records out on the axis. In `even` mode each record takes one unit in branch order, otherwise its real times;
 * a record without times is left out of real-time mode. Requests read their assistant output's request and token times.
 */
export function timeline(records: readonly TrajectoryRecord[], even: boolean): Timeline {
  const bars: TimelineBar[] = [];
  const runs: RunMark[] = [];
  let position = 0;
  let lastRun: string | undefined;
  for (const record of records) {
    const kind = record.kind;
    // An assistant record draws its request's bar; the request record itself only marks prompt changes.
    if (kind.type === "request" && !(kind.promptChanged || kind.toolsAdded.length || kind.toolsRemoved.length)) continue;
    let start = record.startedAtMs;
    let end = record.endedAtMs ?? start;
    let firstToken: number | undefined;
    if (kind.type === "assistant") {
      start = kind.requestedAtMs || start;
      end = kind.lastTokenAtMs || end;
      firstToken = kind.firstTokenAtMs || undefined;
    }
    if (even) {
      // Even spacing keeps the first-token split at its share of the request's real time.
      const share = firstToken !== undefined && start !== undefined && end !== undefined && end > start ? (firstToken - start) / (end - start) : undefined;
      start = position;
      end = position + 1;
      firstToken = share === undefined ? undefined : position + share;
      position++;
    }
    if (start === undefined || end === undefined) continue;
    if (record.run && record.run.id !== lastRun) {
      runs.push({ index: record.run.index, at: start });
      lastRun = record.run.id;
    }
    const lane = kind.type === "assistant" ? "request" : kind.type === "tool" ? "tool" : "event";
    // Events are moments: a prompt change or a user message is a mark where it happened, not a span.
    if (lane === "event" && !even) end = start;
    const failed = (kind.type === "assistant" && (kind.error !== undefined || kind.cancelled)) || (kind.type === "tool" && (kind.isError || kind.cancelled));
    bars.push({ id: record.id, lane, start, end: Math.max(start, end), failed, count: 1, row: 0,
      ...(firstToken === undefined ? {} : { firstToken }) });
  }
  const placed = assignToolRows(bars);
  const starts = placed.map((bar) => bar.start);
  const ends = placed.map((bar) => bar.end);
  const extent = starts.length ? { start: Math.min(...starts), end: Math.max(...ends, Math.min(...starts) + 1) } : { start: 0, end: 1 };
  return { bars: placed, runs, extent };
}

/**
 * Puts each tool bar in the first of `TOOL_ROWS` sub-rows free at its start (greedy by start time). Calls overlapping
 * all rows fold into the bar on the last row, which keeps the total count and any failure. Other bars pass through.
 */
export function assignToolRows(bars: readonly TimelineBar[]): TimelineBar[] {
  const rowEnds: number[] = [];
  // The bar each row last took, by its index in `bars`: an overflowing call merges into the last row's.
  const occupants: number[] = [];
  const tools = bars.map((bar, index) => ({ bar, index })).filter(({ bar }) => bar.lane === "tool").sort((a, b) => a.bar.start - b.bar.start);
  const placed = new Map<number, TimelineBar>();
  for (const { bar, index } of tools) {
    const row = rowEnds.findIndex((end) => end <= bar.start);
    if (row >= 0 || rowEnds.length < TOOL_ROWS) {
      const at = row >= 0 ? row : rowEnds.length;
      rowEnds[at] = bar.end;
      occupants[at] = index;
      placed.set(index, { ...bar, row: at });
      continue;
    }
    // No row is free: fold the call into the bar on the last row, so nothing is drawn over it and its count and
    // failure survive.
    const last = TOOL_ROWS - 1;
    const host = placed.get(occupants[last]!)!;
    const merged: TimelineBar = { ...host, end: Math.max(host.end, bar.end), count: host.count + bar.count, failed: host.failed || bar.failed };
    placed.set(occupants[last]!, merged);
    rowEnds[last] = merged.end;
  }
  return bars.flatMap((bar, index) => (bar.lane !== "tool" ? [bar] : placed.has(index) ? [placed.get(index)!] : []));
}

/** Narrows `window` by `factor` (< 1 zooms in) around the point at `ratio` of its width, kept inside `extent`. */
export function zoomWindow(window: Window, ratio: number, factor: number, extent: Window): Window {
  const width = window.end - window.start;
  const full = extent.end - extent.start;
  const next = Math.min(full, Math.max(full / 1000, width * factor));
  const center = window.start + width * ratio;
  return clampWindow({ start: center - next * ratio, end: center - next * ratio + next }, extent);
}

/** Moves `window` by `delta` of its own width, kept inside `extent`. */
export function panWindow(window: Window, delta: number, extent: Window): Window {
  const shift = (window.end - window.start) * delta;
  return clampWindow({ start: window.start + shift, end: window.end + shift }, extent);
}

/** Slides a window back inside `extent` without changing its width (unless it is wider). */
function clampWindow(window: Window, extent: Window): Window {
  const width = Math.min(window.end - window.start, extent.end - extent.start);
  const start = Math.min(Math.max(window.start, extent.start), extent.end - width);
  return { start, end: start + width };
}

/** The axis value at `x` points of a `width`-point overview showing `window`. */
export function axisAt(x: number, width: number, window: Window): number {
  return window.start + (x / Math.max(1, width)) * (window.end - window.start);
}

/** A bar placed in points: `x` from the overview's left edge, `w` wide. */
export type PlacedBar = TimelineBar & Readonly<{ x: number; w: number }>;

/**
 * The bars inside `window` in points, with neighbours on one row that are both narrower than a point merged, so a
 * long session draws a bounded number of shapes.
 */
export function visibleBars(bars: readonly TimelineBar[], window: Window, width: number): PlacedBar[] {
  const scale = width / Math.max(1e-9, window.end - window.start);
  const out: Array<{ -readonly [K in keyof PlacedBar]: PlacedBar[K] }> = [];
  const lastByRow = new Map<string, (typeof out)[number]>();
  for (const bar of [...bars].sort((a, b) => a.start - b.start)) {
    if (bar.end < window.start || bar.start > window.end) continue;
    const x = (bar.start - window.start) * scale;
    const w = Math.max(0, (bar.end - bar.start) * scale);
    const rowKey = `${bar.lane}:${bar.row}`;
    const last = lastByRow.get(rowKey);
    if (last && w < 1 && last.w < 1 && x - (last.x + last.w) < 1) {
      last.w = Math.max(last.w, x + w - last.x);
      last.count += bar.count;
      last.failed ||= bar.failed;
      continue;
    }
    const placed = { ...bar, x, w };
    out.push(placed);
    lastByRow.set(rowKey, placed);
  }
  return out;
}
