import { computed, effect, Scroll, signal, Text, View } from "nsh";
import { DiffView } from "nsh/base";
import { t } from "nsh/i18n";
import type { JSX } from "nsh/jsx-runtime";

import type { SessionId } from "../client/acp/protocol";
import { messageOf } from "../client/errors";
import { addUsage, NO_USAGE, pairedRecord, previousRequest, TrajectoryRecords } from "../client/trajectory";
import type { TokenUsage, TrajectoryLog, TrajectoryRecord } from "../client/trajectory";
import type { HostStore } from "../stores/host";
import { IconButton } from "../ui/button";
import { CopyButton } from "../ui/copy";
import { Disclosure } from "../ui/disclosure";
import { Empty, Spinner } from "../ui/feedback";
import { ListItem } from "../ui/list";
import { Markdown } from "../ui/markdown";
import { Tooltip } from "../ui/menu";
import { Segmented } from "../ui/selection";
import { Badge } from "../ui/surface";
import { Tabs } from "../ui/tabs";
import { inlineLabel, type } from "../ui/tokens";
import { formatMs, formatTokens, RecordWords } from "./format";

/** The sections a record offers, by its kind. */
type Section = "overview" | "timing" | "usage" | "prompt" | "tools" | "options" | "arguments" | "result" | "raw";

/** Request contents read this run of the app, by Session and hash; they never change. */
const contents = new Map<string, Promise<string | undefined>>();

/** A request content of `sessionId` by hash, read once and shared. */
function requestContent(host: HostStore, sessionId: SessionId, hash: string): Promise<string | undefined> {
  const key = `${host.info.id}:${sessionId}:${hash}`;
  let pending = contents.get(key);
  if (!pending) {
    pending = host.requestContent(sessionId, hash);
    // A failed read is asked again next time.
    pending.catch(() => contents.delete(key));
    contents.set(key, pending);
  }
  return pending;
}

/** Loads one request content into a signal: `undefined` while loading, then the text, missing, or the error. */
function useContent(read: () => Promise<string | undefined> | undefined) {
  const state = signal<{ text?: string; missing?: boolean; error?: string } | undefined>(undefined);
  let version = 0;
  effect(() => {
    const pending = read();
    const mine = ++version;
    state.set(undefined);
    if (!pending) return state.set({ missing: true });
    pending.then(
      (text) => { if (mine === version) state.set(text === undefined ? { missing: true } : { text }); },
      (error: unknown) => { if (mine === version) state.set({ error: messageOf(error) }); },
    );
  });
  return state;
}

/** A label and its value on one line. */
function Field(props: { label: string; value: string }) {
  return (
    <View class="min-h-7 flex-row items-center gap-3">
      <Text class={`w-36 shrink-0 ${inlineLabel} ${type.meta} text-muted-foreground`}>{props.label}</Text>
      <View selectable class="min-w-0 flex-1"><Text class={`tabular-nums ${inlineLabel} ${type.ui} text-foreground`}>{props.value}</Text></View>
    </View>
  );
}

/** Literal text (JSON, a tool result) as it was recorded, selectable, with an inset top-right copy button. */
function Literal(props: { text: string }) {
  return (
    <View class="relative min-h-9 justify-center rounded-(--radius) border border-border/60 bg-muted/65">
      <View selectable class="min-w-0 py-2 pl-3 pr-10">
        <Text class={`font-mono ${type.meta} text-foreground`}>{props.text}</Text>
      </View>
      <View class="absolute right-1.5 top-1.5">
        <CopyButton text={props.text} />
      </View>
    </View>
  );
}

/** Pretty JSON of any value. */
function json(value: unknown): string {
  return JSON.stringify(value, null, 2) ?? "null";
}

/** A wall-clock time, with seconds. */
function clock(ms: number | undefined): string {
  return ms ? new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: "2-digit" }) : "—";
}

/** Usage counters side by side for this request and the branch up to it. */
function UsageTable(props: { request: TokenUsage; total: TokenUsage }) {
  const rows = [["input", "input"], ["cached", "cached"], ["output", "output"], ["reasoning", "reasoning"]] as const;
  const cell = `w-28 shrink-0 text-right tabular-nums ${inlineLabel} ${type.ui} text-foreground`;
  return (
    <View class="gap-1">
      <View class="flex-row items-center gap-3">
        <View class="min-w-0 flex-1" />
        <Text class={`w-28 shrink-0 text-right ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("record.thisRequest")}</Text>
        <Text class={`w-28 shrink-0 text-right ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("record.sessionTotal")}</Text>
      </View>
      {rows.map(([key, label]) => (
        <View class="min-h-7 flex-row items-center gap-3">
          <Text class={`min-w-0 flex-1 ${inlineLabel} ${type.meta} text-muted-foreground`}>{t(`record.${label}`)}</Text>
          <Text class={cell}>{props.request[key].toLocaleString()}</Text>
          <Text class={cell}>{props.total[key].toLocaleString()}</Text>
        </View>
      ))}
    </View>
  );
}

/** One tool definition of a request's list, its schema behind a disclosure. */
function ToolDefinition(props: { tool: { name: string; description?: string; parameters?: unknown } }) {
  const open = signal(false);
  return (
    <Disclosure label={props.tool.name} icon="wrench" open={open()} onToggle={() => open.update((value) => !value)}>
      <View class="gap-2 pl-6">
        <If when={props.tool.description}>{(text) => <Text class={`${type.meta} text-muted-foreground`}>{text()}</Text>}</If>
        <Literal text={json(props.tool.parameters ?? {})} />
      </View>
    </Disclosure>
  );
}

/**
 * The record selected in a Session's trajectory: its kind, title and status, buttons to step through the records,
 * and its sections. A request and its output are one record here.
 */
export function RecordDetails(props: {
  host: HostStore;
  sessionId: SessionId;
  log: TrajectoryLog;
  recordId?: string;
  onSelect: (recordId: string) => void;
}) {
  const record = computed(() => props.log.record(props.recordId));
  // A request and the output it produced share one entry: show them together, the request first.
  const request = computed(() => {
    const current = record();
    if (!current) return undefined;
    return current.kind.type === "request" ? current : pairedRecord(props.log.records(), current);
  });
  const reply = computed(() => {
    const current = record();
    if (!current) return undefined;
    return current.kind.type === "assistant" ? current : pairedRecord(props.log.records(), current);
  });
  const sections = computed<readonly Section[]>(() => {
    const kind = record()?.kind.type;
    if (kind === "request" || kind === "assistant") return ["overview", "timing", "usage", "prompt", "tools", "options", "raw"];
    if (kind === "tool") return ["overview", "arguments", "result", "timing", "raw"];
    return ["overview", "raw"];
  });
  const section = signal<Section>("overview");
  // A record without the chosen section opens at its overview.
  effect(() => {
    if (!sections().includes(section())) section.set("overview");
  });
  const index = computed(() => {
    const current = record();
    return current ? props.log.records().indexOf(current) : -1;
  });
  /**
   * The record `delta` away that shows something new: a request and its output are one view, so the half not
   * selected is stepped over. An unpaired record (an in-flight request, a tool) is never skipped.
   */
  const neighbour = (delta: number): TrajectoryRecord | undefined => {
    const records = props.log.records();
    const shown = (item: TrajectoryRecord) => item === request() || item === reply();
    let at = index() + delta;
    while (at >= 0 && at < records.length && shown(records[at]!)) at += delta;
    return records[at];
  };
  /** Selects the neighbour `delta` away, if any. */
  const step = (delta: number) => {
    const next = neighbour(delta);
    if (next) props.onSelect(next.id);
  };

  const title = computed(() => {
    const current = record();
    if (!current) return "";
    const asked = request();
    if (asked?.kind.type === "request") return RecordWords.content(asked);
    if (current.kind.type === "tool") return current.kind.name;
    return RecordWords.type(current);
  });

  // The branch's usage up to and including this request: before the loaded page, then each loaded output up to it.
  const total = computed(() => {
    // A request still waiting for its output stops the sum at itself.
    const last = reply() ?? request();
    let sum = props.log.usageBefore();
    for (const item of props.log.records()) {
      if (item.kind.type === "assistant") sum = addUsage(sum, item.kind.usage);
      if (item === last) break;
    }
    return sum;
  });

  const promptHash = computed(() => (request()?.kind.type === "request" ? (request()!.kind as { systemPrompt: string }).systemPrompt : undefined));
  const toolsHash = computed(() => (request()?.kind.type === "request" ? (request()!.kind as { tools: string }).tools : undefined));
  const earlier = computed(() => {
    const asked = request();
    return asked ? previousRequest(props.log.records(), asked) : undefined;
  });
  const earlierPromptHash = computed(() => {
    const kind = earlier()?.kind;
    return kind?.type === "request" && kind.systemPrompt !== promptHash() ? kind.systemPrompt : undefined;
  });
  const prompt = useContent(() => (section() === "prompt" && promptHash() ? requestContent(props.host, props.sessionId, promptHash()!) : undefined));
  const earlierPrompt = useContent(() => (section() === "prompt" && earlierPromptHash()
    ? requestContent(props.host, props.sessionId, earlierPromptHash()!) : undefined));
  const tools = useContent(() => (section() === "tools" && toolsHash() ? requestContent(props.host, props.sessionId, toolsHash()!) : undefined));
  const promptMode = signal<"full" | "diff">("full");
  // Outside the section body, which rebuilds as records catch up, so the disclosure stays as the reader left it.
  const thinking = signal(false);
  const toolList = computed(() => {
    const text = tools()?.text;
    if (text === undefined) return [];
    try {
      const parsed: unknown = JSON.parse(text);
      return Array.isArray(parsed) ? parsed.flatMap((item: unknown) => {
        const tool = item as { name?: unknown; description?: unknown; parameters?: unknown };
        return typeof tool?.name === "string"
          ? [{ name: tool.name, ...(typeof tool.description === "string" ? { description: tool.description } : {}), parameters: tool.parameters }] : [];
      }) : [];
    } catch {
      return [];
    }
  });
  /** A content still loading, missing, or failed; `undefined` once it has text. */
  const contentState = (state: { text?: string; missing?: boolean; error?: string } | undefined): JSX.Element | undefined => {
    if (state === undefined) return <View class="items-center p-6"><Spinner label={t("record.loading")} /></View>;
    if (state.error !== undefined) return <Empty icon="circle-alert" title={state.error} />;
    if (state.missing) return <Empty icon="file-text" title={t("record.missing")} />;
    return undefined;
  };

  /** The selected section of the shown record. */
  const body = (current: TrajectoryRecord): JSX.Element => {
    const kind = current.kind;
    const output = reply();
    const asked = request();
    switch (section()) {
      case "overview": {
        if (output?.kind.type === "assistant") {
          const answer = output.kind;
          return (
            <View class="gap-3">
              <If when={answer.reasoning}>
                <Disclosure flush label={t("record.thinking")} icon="brain" open={thinking()} onToggle={() => thinking.update((value) => !value)}>
                  <Markdown text={answer.reasoning} tone="muted" />
                </Disclosure>
              </If>
              <If when={answer.text} else={<Text class={`${type.meta} text-muted-foreground`}>{t("record.noText")}</Text>}>
                <Markdown text={answer.text} />
              </If>
              <If when={answer.error}>{(error) => <View selectable><Text class={`${type.meta} text-destructive`}>{error()}</Text></View>}</If>
              <If when={answer.toolCallIds.length > 0}>
                <View class="gap-0.5">
                  <Text role="header" class={`${type.meta} font-medium text-muted-foreground`}>{t("record.calls")}</Text>
                  {answer.toolCallIds.map((callId) => {
                    const call = props.log.records().find((item) => item.kind.type === "tool" && item.kind.callId === callId);
                    return <ListItem icon="wrench" label={call ? RecordWords.content(call) : callId} disabled={!call}
                      onPress={() => call && props.onSelect(call.id)} />;
                  })}
                </View>
              </If>
            </View>
          );
        }
        if (kind.type === "tool") {
          return (
            <View class="gap-1">
              <Field label={t("trajectory.types.tool")} value={kind.name} />
              <Field label="ID" value={kind.callId} />
              <Field label={t("record.duration")} value={TrajectoryRecords.duration(current) === undefined ? "—" : formatMs(TrajectoryRecords.duration(current)!)} />
            </View>
          );
        }
        if (kind.type === "user") return <View selectable><Text class={`${type.body} text-foreground`}>{kind.text}</Text></View>;
        if (kind.type === "compaction" || kind.type === "branch_summary") return <Markdown text={kind.summary} />;
        if (kind.type === "request") return <Text class={`${type.meta} text-muted-foreground`}>{RecordWords.system(current) || RecordWords.content(current)}</Text>;
        return <View selectable><Text class={`${type.ui} text-foreground`}>{RecordWords.content(current)}</Text></View>;
      }
      case "timing": {
        if (output?.kind.type === "assistant") {
          const answer = output.kind;
          const waited = answer.firstTokenAtMs - answer.requestedAtMs;
          const generated = answer.lastTokenAtMs - answer.firstTokenAtMs;
          return (
            <View class="gap-1">
              <Field label={t("record.sent")} value={clock(answer.requestedAtMs)} />
              <Field label={t("record.firstToken")} value={clock(answer.firstTokenAtMs)} />
              <Field label={t("record.lastToken")} value={clock(answer.lastTokenAtMs)} />
              <Field label={t("record.waited")} value={formatMs(waited)} />
              <Field label={t("record.generated")} value={formatMs(generated)} />
              <Field label={t("record.throughput")} value={generated > 0 ? t("trajectory.hover.throughput", { rate: (answer.usage.output / (generated / 1000)).toFixed(1) }) : "—"} />
            </View>
          );
        }
        return (
          <View class="gap-1">
            <Field label={t("record.started")} value={clock(current.startedAtMs)} />
            <Field label={t("record.ended")} value={clock(current.endedAtMs)} />
            <Field label={t("record.duration")} value={TrajectoryRecords.duration(current) === undefined ? "—" : formatMs(TrajectoryRecords.duration(current)!)} />
          </View>
        );
      }
      case "usage":
        return <UsageTable request={output?.kind.type === "assistant" ? output.kind.usage : NO_USAGE} total={total()} />;
      case "prompt":
        return contentState(prompt()) ?? (
          <View class="min-h-0 flex-1 gap-2.5">
            <If when={earlierPromptHash() && earlierPrompt()?.text !== undefined}>
              <Segmented class="self-start" items={[{ value: "full", label: t("record.full") }, { value: "diff", label: t("record.diff") }]}
                value={promptMode()} accessibilityLabel={t("record.promptView")} onChange={(value) => promptMode.set(value === "diff" ? "diff" : "full")} />
            </If>
            <If when={promptMode() === "diff" && earlierPrompt()?.text !== undefined} else={<Literal text={prompt()?.text ?? ""} />}>
              <DiffView class="min-h-96 overflow-hidden rounded-(--radius) border border-border bg-background" layout="unified" indicators="classic" contextLines={3} softWrap
                oldValue={earlierPrompt()?.text ?? ""} newValue={prompt()?.text ?? ""}
                classes={{ lineAdded: "bg-added/10", lineDeleted: "bg-removed/10", indicatorAdded: "text-added",
                  indicatorDeleted: "text-removed", separator: "bg-muted", lineNumber: "text-muted-foreground" }} />
            </If>
          </View>
        );
      case "tools": {
        const changes = asked?.kind.type === "request" ? asked.kind : undefined;
        return contentState(tools()) ?? (
          <View class="gap-2">
            <If when={changes && changes.toolsAdded.length > 0}>
              <Field label={t("record.added")} value={changes?.toolsAdded.join(", ") ?? ""} />
            </If>
            <If when={changes && changes.toolsRemoved.length > 0}>
              <Field label={t("record.removed")} value={changes?.toolsRemoved.join(", ") ?? ""} />
            </If>
            <Each of={toolList()} key={(tool) => tool.name}>{(tool) => <ToolDefinition tool={tool()} />}</Each>
          </View>
        );
      }
      case "options":
        return <Literal text={json(asked?.kind.type === "request" ? { ...asked.kind.options, messageCount: asked.kind.messageCount } : {})} />;
      case "arguments":
        return <Literal text={kind.type === "tool" ? json(kind.arguments) : ""} />;
      case "result":
        return kind.type === "tool" && kind.resultText !== undefined ? <Literal text={kind.resultText} />
          : <Text class={`${type.meta} text-muted-foreground`}>{t("record.noResult")}</Text>;
      case "raw":
        return <Literal text={json(asked && output && asked !== output ? [asked.raw, output.raw] : current.raw)} />;
    }
  };

  return (
    <If when={record()} else={<Empty class="flex-1" icon="chart-gantt" title={t("record.none")} />}>
      {(current) => (
        <View class="min-h-0 flex-1">
          <View class="gap-1.5 border-b border-border px-4 py-3">
            <View class="flex-row items-center gap-2">
              <Badge label={request() ? t("trajectory.types.request") : RecordWords.type(current())} />
              <Badge label={RecordWords.status(reply() ?? current(), props.log.pending(reply() ?? current())).label}
                tone={RecordWords.status(reply() ?? current(), props.log.pending(reply() ?? current())).tone} />
              <View class="flex-1" />
              <IconButton icon="chevron-left" label={t("record.previous")} size="sm" disabled={neighbour(-1) === undefined}
                tooltip={<Tooltip text={t("record.previous")} />} onPress={() => step(-1)} />
              <IconButton icon="chevron-right" label={t("record.next")} size="sm" disabled={neighbour(1) === undefined}
                tooltip={<Tooltip text={t("record.next")} />} onPress={() => step(1)} />
            </View>
            <Text class={`${type.ui} font-semibold text-foreground line-clamp-2`}>{title()}</Text>
            <If when={reply()?.kind.type === "assistant" && (reply()!.kind as { usage: TokenUsage }).usage.input > 0}>
              <Text class={`tabular-nums ${inlineLabel} ${type.meta} text-muted-foreground`}>
                {t("record.tokenFlow", {
                  input: formatTokens((reply()!.kind as { usage: TokenUsage }).usage.input),
                  output: formatTokens((reply()!.kind as { usage: TokenUsage }).usage.output),
                })}
              </Text>
            </If>
          </View>
          {/* A narrow panel cannot fit every section: the tabs scroll sideways instead of clipping the last ones. */}
          <Scroll horizontal class="h-10 shrink-0 grow-0 border-b border-border" contentClass="h-10 flex-row px-2">
            <Tabs variant="bar" items={sections().map((value) => ({ value, label: t(`record.tabs.${value}`) }))} value={section()}
              accessibilityLabel={t("record.sections")} onChange={(value) => section.set(value as Section)} />
          </Scroll>
          <Scroll class="min-h-0 flex-1" contentClass="gap-3 p-4">
            {body(current())}
          </Scroll>
        </View>
      )}
    </If>
  );
}
