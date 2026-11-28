import { cleanup, computed, effect, keymap, List, signal, Text, View } from "nsh";
import type { ListRef } from "nsh";
import { t } from "nsh/i18n";

import type { SessionId } from "../client/acp/protocol";
import { overlaps, runHeadingKey, shownRecords, summarize, trajectoryRows, TrajectoryRecords } from "../client/trajectory";
import type { TrajectoryLog, TrajectoryRow, Window } from "../client/trajectory";
import type { HostStore } from "../stores/host";
import { Appear } from "../ui/appear";
import { Button, IconButton } from "../ui/button";
import { Empty, Skeleton, Spinner } from "../ui/feedback";
import { Icon } from "../ui/icon";
import { Tooltip } from "../ui/menu";
import { motionClass } from "../ui/motion";
import { Checkbox, Segmented } from "../ui/selection";
import { Badge, Separator } from "../ui/surface";
import { focusRing, inlineLabel, type } from "../ui/tokens";
import { formatMs, formatTokens, RecordWords } from "./format";
import { Timeline } from "./Timeline";
import { mainWindowFocused } from "../ui/windowFocus";

/** Every row of the table is this tall, so the list scrolls and selects by index without measuring. */
const ROW_HEIGHT = 32;
/** Within this many points of the end the table follows new records. */
const BOTTOM_THRESHOLD = 48;
/** Session updates within this long of each other are caught up with one query. */
const REFRESH_DEBOUNCE_MS = 150;

/** Whether the overview shows; shared by every Session, as a layout choice. */
const timelineShown = signal(true);

/** The numeric columns of a row: input, output and reasoning tokens, and the duration. */
function NumberCells(props: { input?: number; output?: number; reasoning?: number; duration?: number }) {
  const cell = `w-14 shrink-0 text-right tabular-nums ${inlineLabel} ${type.meta} text-muted-foreground`;
  return (
    <>
      <Text class={cell}>{props.input === undefined ? "" : formatTokens(props.input)}</Text>
      <Text class={cell}>{props.output === undefined ? "" : formatTokens(props.output)}</Text>
      <Text class={cell}>{props.reasoning ? formatTokens(props.reasoning) : ""}</Text>
      <Text class={`${cell} w-16`}>{props.duration === undefined ? "" : formatMs(props.duration)}</Text>
    </>
  );
}

/** One table row; selection uses the native `sidebar-accent` surface, while Run headers and Request separators form a clear two-tier hierarchy. */
function Row(props: { row: TrajectoryRow; selected: boolean; onPress: () => void }) {
  const base = `h-8 flex-row items-center gap-2 px-3 ${focusRing} ${motionClass.wash}`;
  const surface = () => {
    if (props.selected) return "bg-sidebar-accent active:bg-sidebar-accent/80";
    if (props.row.type === "run") return "border-t border-border bg-muted/55 hover:bg-accent active:bg-sidebar-accent/80";
    if (props.row.type === "request") return "border-t border-border/60 bg-muted/20 hover:bg-accent/70 active:bg-sidebar-accent/70";
    return "hover:bg-accent/60 active:bg-sidebar-accent/70";
  };
  return (
    <View
      role="button"
      accessibilityState={{ selected: props.selected }}
      class={`${base} ${surface()}`}
      onPress={props.onPress}
    >
      {(() => {
        const row = props.row;
        if (row.type === "run") {
          return (
            <>
              <View class={`w-24 shrink-0 flex-row items-center gap-1.5`}>
                <View class={`${row.collapsed ? "" : "rotate-90"} ${motionClass.turn}`}>
                  <Icon name="chevron-right" size="sm" color="muted-foreground" />
                </View>
                <Text class={`shrink-0 ${inlineLabel} ${type.ui} font-semibold text-foreground`}>
                  {row.index ? t("trajectory.run", { n: row.index }) : t("trajectory.betweenRuns")}
                </Text>
              </View>
              <View class="min-w-0 flex-1 flex-row items-center gap-2">
                {/* The Run's total sits in the Content column rather than under Input. */}
                <If when={row.tokens > 0}>
                  <Text class={`min-w-0 truncate tabular-nums ${inlineLabel} ${type.meta} text-muted-foreground`}>
                    {t("trajectory.runTokens", { tokens: formatTokens(row.tokens) })}
                  </Text>
                </If>
              </View>
              <NumberCells duration={row.durationMs} />
            </>
          );
        }
        if (row.type === "tools") {
          return (
            <>
              <Text class={`w-24 shrink-0 ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("trajectory.types.tool")}</Text>
              <View class="min-w-0 flex-1 flex-row items-center gap-1.5">
                <View class={`${row.open ? "rotate-90" : ""} ${motionClass.turn}`}>
                  <Icon name="chevron-right" size="sm" color="muted-foreground" />
                </View>
                <Text class={`shrink-0 ${inlineLabel} ${type.ui} text-foreground`}>{t("trajectory.toolCalls", { count: row.count })}</Text>
                <If when={row.failed > 0}>
                  <Icon name="circle-alert" size="sm" color="destructive" />
                  <Text class={`min-w-0 truncate ${inlineLabel} ${type.ui} text-destructive`}>{t("trajectory.failedCount", { count: row.failed })}</Text>
                </If>
              </View>
              <NumberCells duration={row.durationMs} />
            </>
          );
        }
        const record = row.record;
        const kind = record.kind;
        if (row.type === "request" && kind.type === "request") {
          const changed = kind.promptChanged || kind.toolsAdded.length > 0 || kind.toolsRemoved.length > 0;
          return (
            <>
              <Text class={`w-24 shrink-0 ${inlineLabel} ${type.meta} font-medium ${props.selected ? "text-foreground" : "text-muted-foreground"}`}>{t("trajectory.types.request")}</Text>
              <View class="min-w-0 flex-1 flex-row items-center gap-2">
                {/* What changed is the reason to look at a request, so it leads and never truncates away. */}
                <If when={changed}><Badge label={t("trajectory.changed")} tone="info" /></If>
                <Text class={`min-w-0 truncate ${inlineLabel} ${type.ui} font-medium text-foreground`}>
                  {[t("trajectory.requestShort", { n: kind.index, model: kind.options.modelId }),
                    row.firstTokenMs === undefined ? "" : t("trajectory.firstToken", { duration: formatMs(row.firstTokenMs) })].filter(Boolean).join(" · ")}
                </Text>
              </View>
              <NumberCells />
            </>
          );
        }
        const failed = RecordWords.failed(record);
        return (
          <>
            <Text class={`w-24 shrink-0 ${inlineLabel} ${type.meta} ${props.selected ? "font-medium text-foreground" : "text-muted-foreground"}`}>
              {row.type === "system" ? t("trajectory.types.system") : RecordWords.type(record)}
            </Text>
            <View class={`min-w-0 flex-1 flex-row items-center ${row.type === "record" && row.inGroup ? "border-l border-border pl-3" : ""}`}>
              <Text class={`min-w-0 flex-1 truncate ${inlineLabel} ${type.ui} ${props.selected ? "font-medium" : ""} ${failed ? "text-destructive" : "text-foreground"}`}>
                {row.type === "system" ? RecordWords.system(record) : RecordWords.content(record)}
              </Text>
            </View>
            <NumberCells
              input={kind.type === "assistant" ? kind.usage.input : undefined}
              output={kind.type === "assistant" ? kind.usage.output : undefined}
              reasoning={kind.type === "assistant" ? kind.usage.reasoning : undefined}
              // Only answers and tool calls span time; a user message or setting is an instant.
              duration={row.type !== "system" && (kind.type === "assistant" || kind.type === "tool") ? TrajectoryRecords.duration(record) : undefined} />
          </>
        );
      })()}
    </View>
  );
}

/**
 * A Session's trajectory: a toolbar, the time overview and the record table, newest at the bottom. Pressing a record
 * selects it, which shows it in the record details. While shown it catches up after the Session's messages and tools
 * complete and after each turn.
 */
export function TrajectoryView(props: {
  host: HostStore;
  sessionId: SessionId;
  log: TrajectoryLog;
  selectedId?: string;
  onSelect: (recordId: string) => void;
  /** A modal dialog sits above the Session, so keys belong to it. */
  modalOpen?: boolean;
}) {
  const log = props.log;
  const collapsedRuns = signal<ReadonlySet<string>>(new Set());
  const groupTools = signal(true);
  const openGroups = signal<ReadonlySet<string>>(new Set());
  const even = signal(false);
  const range = signal<Window | undefined>(undefined);
  const atBottom = signal(true);
  let list: ListRef | undefined;

  // The first time the log is shown it loads its tail; when shown again it catches up once.
  if (log.records().length === 0 && !log.failure()) void log.load();
  else void log.refresh();

  // Catch up when a message or tool completes and when a turn ends: the transcript grows, a tool settles or the
  // Session stops running. Bursts within the debounce make one query.
  const session = computed(() => props.host.session(props.sessionId));
  const running = computed(() => props.host.running(props.sessionId));
  const progress = computed(() => {
    const current = session();
    if (!current) return "";
    let settled = 0;
    for (const tool of current.tools.values()) if (tool.status !== "pending" && tool.status !== "in_progress") settled++;
    return `${current.transcript.length}:${settled}:${running()}`;
  });
  let first = true;
  effect(() => {
    progress();
    if (first) {
      first = false;
      return;
    }
    const timer = setTimeout(() => void log.refresh(), REFRESH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  });

  const rows = computed(() => trajectoryRows(log.records(), {
    collapsedRuns: collapsedRuns(), groupTools: groupTools(), openGroups: openGroups(), ...(range() ? { range: range()! } : {}),
  }));
  const summary = computed(() => summarize(log.records()));
  const allFolded = computed(() => rows().filter((row) => row.type === "run").every((row) => row.type === "run" && row.collapsed));
  // What runs now started after the last loaded record ended; the newest record's end, or the newest start.
  const runningSince = computed(() => {
    if (!running()) return undefined;
    const last = log.records().at(-1);
    return last?.endedAtMs ?? last?.startedAtMs ?? Date.now();
  });
  const now = signal(Date.now());
  effect(() => {
    if (runningSince() === undefined) return;
    now.set(Date.now());
    const timer = setInterval(() => now.set(Date.now()), 1000);
    return () => clearInterval(timer);
  });

  /** Selects a record from the overview and scrolls the table to its row (its Run or group opened first). */
  const reveal = (recordId: string) => {
    const record = log.records().find((item) => item.id === recordId);
    if (!record) return;
    const shown = (): number => rows().findIndex((row) => RecordWords.ofRow(row)?.id === recordId);
    if (shown() < 0) {
      // Open what hides it: a time range it lies outside, its own heading, and its tool group.
      if (range() && !overlaps(record, range()!)) range.set(undefined);
      const heading = runHeadingKey(shownRecords(log.records(), range()), record);
      collapsedRuns.update((set) => new Set([...set].filter((key) => key !== heading)));
      const group = rows().find((row) => row.type === "tools" && !row.open);
      if (shown() < 0 && group) groupTools.set(false);
    }
    props.onSelect(recordId);
    const index = shown();
    if (index >= 0) list?.scrollToIndex({ index, animated: true, viewPosition: 0.5 });
  };

  // Up and Down move the selection through the records while no text field, and no other window, has the keys.
  effect(() => {
    if (props.modalOpen || !mainWindowFocused()) return;
    const step = (delta: number) => {
      // A system row shows its request's record again: stepping onto it would select the same record and stall there.
      const selectable = rows().flatMap((row, index) => (RecordWords.ofRow(row) && row.type !== "system" ? [{ row, index }] : []));
      const at = selectable.findIndex(({ row }) => RecordWords.ofRow(row)?.id === props.selectedId);
      const next = selectable[at < 0 ? (delta > 0 ? 0 : selectable.length - 1) : Math.min(selectable.length - 1, Math.max(0, at + delta))];
      if (!next) return;
      props.onSelect(RecordWords.ofRow(next.row)!.id);
      list?.scrollToIndex({ index: next.index, animated: false, viewPosition: 0.5 });
    };
    return keymap.bind({ up: () => step(-1), down: () => step(1) }, { when: "notEditing" });
  });

  /** Presses a row: a Run heading folds, a tool group opens, a record is selected. */
  const press = (row: TrajectoryRow) => {
    if (row.type === "run") {
      collapsedRuns.update((set) => {
        const next = new Set(set);
        if (!next.delete(row.key)) next.add(row.key);
        return next;
      });
      return;
    }
    if (row.type === "tools") {
      openGroups.update((set) => {
        const next = new Set(set);
        if (!next.delete(row.key)) next.add(row.key);
        return next;
      });
      return;
    }
    props.onSelect(row.record.id);
  };

  /** The range's ends as offsets from the first loaded record. */
  const rangeLabel = computed(() => {
    const current = range();
    const origin = log.records().find((record) => record.startedAtMs !== undefined)?.startedAtMs;
    if (!current || origin === undefined) return undefined;
    return t("trajectory.range", { from: `+${formatMs(current.start - origin)}`, to: `+${formatMs(current.end - origin)}` });
  });

  return (
    <View class="min-h-0 flex-1">
      <View class="min-h-11 flex-row flex-wrap items-center gap-2 border-b border-border px-3 py-1.5">
        {/* Time spacing is a choice of two, so it reads as a segmented control; the folds are plain on/off checks. */}
        <Segmented accessibilityLabel={t("trajectory.spacing")} value={even() ? "even" : "real"}
          items={[{ value: "real", label: t("trajectory.realTime") }, { value: "even", label: t("trajectory.evenSpacing") }]}
          onChange={(value) => even.set(value === "even")} />
        <View class="h-3.5"><Separator vertical /></View>
        <View class="flex-row items-center gap-3 px-0.5">
          <Checkbox label={t("trajectory.foldRuns")} checked={allFolded() && rows().length > 0} onChange={(checked) => {
            collapsedRuns.set(checked ? new Set(rows().flatMap((row) => (row.type === "run" ? [row.key] : []))) : new Set());
          }} />
          <Checkbox label={t("trajectory.groupCalls")} checked={groupTools()} onChange={(checked) => groupTools.set(checked)} />
        </View>
        <View class="h-3.5"><Separator vertical /></View>
        <IconButton icon="chart-gantt" label={t("trajectory.showTimeline")} size="sm" selected={timelineShown()}
          tooltip={<Tooltip text={t("trajectory.showTimeline")} />} onPress={() => timelineShown.update((value) => !value)} />
        <If when={log.hasMoreBefore()}>
          <Button label={t("trajectory.loadAll")} size="sm" variant="ghost" loading={log.loadingEarlier()} onPress={() => void log.loadAll()} />
        </If>
        <If when={rangeLabel()}>
          {(label) => (
            <View class="flex-row items-center gap-0.5 rounded-(--radius) border border-border bg-muted/70 py-0.5 pl-2.5 pr-0.5">
              <Text class={`tabular-nums ${inlineLabel} ${type.meta} text-foreground`}>{label()}</Text>
              <IconButton icon="x" label={t("trajectory.clearRange")} size="sm" onPress={() => range.set(undefined)} />
            </View>
          )}
        </If>
        {/* The summary wraps to its own line rather than truncating away the cache rate. */}
        <View class="shrink-0 grow items-end">
          <Text class={`whitespace-nowrap tabular-nums ${inlineLabel} ${type.meta} text-muted-foreground`}>
            {[t("trajectory.summary", { runs: summary().runs, requests: summary().requests, tokens: formatTokens(summary().tokens) }),
              summary().cacheHit === undefined ? "" : t("trajectory.cacheHit", { rate: Math.round(summary().cacheHit! * 100) })].filter(Boolean).join(" · ")}
          </Text>
        </View>
      </View>
      <If when={timelineShown() && log.records().length > 0}>
        <Timeline records={log.records()} even={even()} truncated={log.hasMoreBefore()} runningSince={runningSince()}
          selectedId={props.selectedId} range={range()} onSelect={reveal} onRange={(next) => range.set(next)} />
      </If>
      <View class="h-8 flex-row items-center gap-2 border-b border-border px-3">
        <Text class={`w-24 shrink-0 ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("trajectory.columns.type")}</Text>
        <Text class={`min-w-0 flex-1 ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("trajectory.columns.content")}</Text>
        {(["input", "output", "thinking"] as const).map((column) => (
          <Text class={`w-14 shrink-0 text-right ${inlineLabel} ${type.meta} text-muted-foreground`}>{t(`trajectory.columns.${column}`)}</Text>
        ))}
        <Text class={`w-16 shrink-0 text-right ${inlineLabel} ${type.meta} text-muted-foreground`}>{t("trajectory.columns.duration")}</Text>
      </View>
      <View class="min-h-0 flex-1">
        <If when={!log.loading() || log.records().length > 0} else={
          <View class="gap-2 p-3">{[0, 1, 2, 3, 4, 5].map(() => <Skeleton class="h-5 w-full" />)}</View>
        }>
          <If when={log.failure()} else={
            <If when={log.records().length > 0} else={<Empty class="flex-1" icon="chart-gantt" title={t("trajectory.empty")} />}>
              <List
                ref={list}
                accessibilityLabel={t("trajectory.records")}
                each={rows()}
                keyExtractor={(row) => row.key}
                estimatedItemSize={ROW_HEIGHT}
                initialScrollAtEnd
                maintainVisibleContentPosition={{ autoscrollToBottomThreshold: BOTTOM_THRESHOLD }}
                onScroll={(event) => atBottom.set(event.contentSize.height - (event.offset.y + event.size.height) <= BOTTOM_THRESHOLD)}
                onLoadMoreAtStart={() => void log.loadEarlier()}
                class="flex-1"
                header={
                  <If when={log.hasMoreBefore()}>
                    <View class="h-8 flex-row items-center px-3">
                      <Button label={t("trajectory.loadEarlier")} size="sm" variant="ghost" loading={log.loadingEarlier()} onPress={() => void log.loadEarlier()} />
                    </View>
                  </If>
                }
                footer={
                  <View>
                    <If when={log.refreshFailure()}>
                      <View role="alert" class="h-8 flex-row items-center gap-2 px-3">
                        <Icon name="circle-alert" size="sm" color="destructive" />
                        <Text class={`min-w-0 flex-1 truncate ${inlineLabel} ${type.meta} text-destructive`}>{t("trajectory.refreshFailed")}</Text>
                        <Button label={t("trajectory.retry")} size="sm" variant="ghost" onPress={() => void log.refresh()} />
                      </View>
                    </If>
                    <If when={runningSince()}>
                      {(since) => (
                        <View class="h-8 flex-row items-center gap-2 px-3">
                          <Spinner label={t("record.status.running")} size="sm" />
                          <Text class={`${inlineLabel} ${type.meta} text-muted-foreground`}>
                            {t("trajectory.inProgress", { duration: formatMs(now() - since()) })}
                          </Text>
                        </View>
                      )}
                    </If>
                  </View>
                }
              >
                {(row) => <Row row={row()}
                  // Runs and tool groups stand for no record: with nothing selected, `undefined === undefined` must not select them.
                  selected={props.selectedId !== undefined && RecordWords.ofRow(row())?.id === props.selectedId && row().type !== "system"}
                  onPress={() => press(row())} />}
              </List>
              <If when={!atBottom()}>
                <Appear entrance="riseSm" class="absolute bottom-3 right-3">
                  <IconButton icon="arrow-down" label={t("trajectory.jumpToLatest")} variant="outline" round class="shadow-md"
                    onPress={() => {
                      list?.scrollToEnd({ animated: true });
                      atBottom.set(true);
                    }} />
                </Appear>
              </If>
            </If>
          }>
            {(failure) => (
              <Empty class="flex-1" icon="circle-alert"
                title={failure().kind === "unavailable" ? t("trajectory.unavailable") : t("trajectory.failedToLoad")}
                description={failure().kind === "unavailable" ? undefined : failure().detail}
                action={<Button label={t("trajectory.retry")} size="sm" onPress={() => void log.load()} />} />
            )}
          </If>
        </If>
      </View>
    </View>
  );
}
