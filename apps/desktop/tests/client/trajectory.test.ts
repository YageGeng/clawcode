import { expect, test } from "nsh/test";

import {
  assignToolRows, decodeTrajectoryPage, runHeadingKey, panWindow, summarize, timeline, TrajectoryLog, trajectoryRows, visibleBars, zoomWindow,
} from "../../src/client/trajectory";
import type { TimelineBar, TrajectoryQuery, TrajectoryRecord } from "../../src/client/trajectory";
import { usage, wireRun } from "../support/trajectoryRecords";

/** Decodes wire records through a page. */
function decoded(records: unknown[]): readonly TrajectoryRecord[] {
  return decodeTrajectoryPage({ records }).records;
}

test("pages_decode_known_kinds_and_skip_unknown_ones", () => {
  const page = decodeTrajectoryPage({
    records: [...wireRun(1, 1_000, 1), { id: "x", sequence: 9, kind: { type: "future" } }],
    hasMoreBefore: true, firstSequence: 1, lastSequence: 5, usageBefore: usage(10, 2),
  });
  expect(page.records.length).toBe(7);
  expect(page.hasMoreBefore).toBe(true);
  expect(page.usageBefore).toEqual({ input: 10, output: 2, cached: 0, reasoning: 0 });
  const answer = page.records[6]!.kind;
  expect(answer.type === "assistant" && answer.firstTokenAtMs).toBe(2_700);
  expect(() => decodeTrajectoryPage({})).toThrow();
});

test("rows_head_each_run_separate_requests_and_fold_tool_calls", () => {
  const records = decoded([...wireRun(1, 1_000, 1), ...wireRun(2, 10_000, 10)]);
  const rows = trajectoryRows(records, { collapsedRuns: new Set(), groupTools: true, openGroups: new Set() });
  expect(rows.map((row) => row.type)).toEqual([
    "run", "record", "request", "system", "record", "tools", "request", "system", "record",
    "run", "record", "request", "record", "tools", "request", "system", "record",
  ]);
  const heading = rows[0]!;
  expect(heading.type === "run" && heading.durationMs).toBe(2_000);
  expect(heading.type === "run" && heading.tokens).toBe(2_170);
  const tools = rows[5]!;
  expect(tools.type === "tools" && [tools.count, tools.failed, tools.durationMs]).toEqual([2, 1, 500]);
  const request = rows[2]!;
  expect(request.type === "request" && request.firstTokenMs).toBe(390);
  // Opening the group lists its calls; folding the Run leaves only its heading.
  const opened = trajectoryRows(records, { collapsedRuns: new Set([heading.key]), groupTools: true, openGroups: new Set([tools.key]) });
  expect(opened[0]!.type).toBe("run");
  expect(opened[1]!.type).toBe("run");
  // Calls listed under an opened group are marked as its members; ungrouped calls are not.
  const listed = trajectoryRows(records, { collapsedRuns: new Set(), groupTools: true, openGroups: new Set([tools.key]) });
  expect(listed.filter((row) => row.type === "record" && row.inGroup).length).toBe(2);
  const ungrouped = trajectoryRows(records, { collapsedRuns: new Set(), groupTools: false, openGroups: new Set() });
  expect(ungrouped.filter((row) => row.type === "tools").length).toBe(0);
  expect(ungrouped.filter((row) => row.type === "record" && row.record.kind.type === "tool").length).toBe(4);
  expect(ungrouped.some((row) => row.type === "record" && row.inGroup)).toBe(false);
});

test("a_time_range_keeps_only_overlapping_records", () => {
  const records = decoded(wireRun(1, 1_000, 1));
  const rows = trajectoryRows(records, { collapsedRuns: new Set(), groupTools: false, openGroups: new Set(), range: { start: 2_100, end: 2_300 } });
  expect(rows.flatMap((row) => (row.type === "record" ? [row.record.id] : []))).toEqual(["tool:c1-1", "tool:c2-1"]);
});

test("summary_counts_runs_requests_tokens_and_cache_hits", () => {
  const summary = summarize(decoded([...wireRun(1, 1_000, 1), ...wireRun(2, 10_000, 10)]));
  expect(summary.runs).toBe(2);
  expect(summary.requests).toBe(4);
  expect(summary.tokens).toBe(4_340);
  expect(summary.cacheHit).toBe(3_600 / 4_200);
});

test("tool_calls_share_three_rows_and_overflow_merges", () => {
  /** A tool bar from `start` to `end`. */
  const bar = (id: string, start: number, end: number): TimelineBar => ({ id, lane: "tool", start, end, failed: false, count: 1, row: 0 });
  const placed = assignToolRows([bar("a", 0, 10), bar("b", 1, 10), bar("c", 2, 10), bar("d", 3, 10), bar("e", 4, 10), bar("f", 11, 12)]);
  // d and e find no free row: they fold into c on the last row instead of being drawn over it.
  expect(placed.map((item) => [item.id, item.row, item.count])).toEqual([["a", 0, 1], ["b", 1, 1], ["c", 2, 3], ["f", 0, 1]]);
  // A failed call keeps its failure when a successful one folds into it, and the other way round.
  const failed = assignToolRows([bar("a", 0, 10), bar("b", 0, 10), { ...bar("c", 0, 10), failed: true }, bar("d", 0, 10)]);
  expect(failed.map((item) => [item.id, item.row, item.count, item.failed])).toEqual([["a", 0, 1, false], ["b", 1, 1, false], ["c", 2, 2, true]]);
});

test("the_timeline_uses_request_times_and_even_spacing", () => {
  const records = decoded(wireRun(1, 1_000, 1));
  const real = timeline(records, false);
  const request = real.bars.find((bar) => bar.id === "e1a")!;
  expect([request.lane, request.start, request.firstToken, request.end]).toEqual(["request", 1_010, 1_400, 2_000]);
  expect(real.runs).toEqual([{ index: 1, at: 1_000 }]);
  expect(real.bars.find((bar) => bar.id === "tool:c2-1")!.failed).toBe(true);
  // The second request changed tools, so its request record marks an event.
  expect(real.bars.some((bar) => bar.id === "request:e1b" && bar.lane === "event")).toBe(true);
  const even = timeline(records, true);
  expect(even.extent).toEqual({ start: 0, end: 6 });
});

test("zoom_and_pan_stay_inside_the_extent", () => {
  const extent = { start: 0, end: 100 };
  const zoomed = zoomWindow(extent, 0.5, 0.5, extent);
  expect(zoomed).toEqual({ start: 25, end: 75 });
  expect(panWindow(zoomed, 1, extent)).toEqual({ start: 50, end: 100 });
  expect(zoomWindow(zoomed, 0.5, 4, extent)).toEqual(extent);
});

test("thin_neighbours_on_one_row_merge", () => {
  const bars: TimelineBar[] = [0, 1, 2].map((i) => ({ id: `b${i}`, lane: "request", start: i * 0.1, end: i * 0.1 + 0.05, failed: i === 2, count: 1, row: 0 }));
  const shown = visibleBars(bars, { start: 0, end: 1000 }, 100);
  expect(shown.length).toBe(1);
  expect([shown[0]!.count, shown[0]!.failed]).toEqual([3, true]);
});

test("the_log_pages_back_and_catches_up_from_a_pending_tool", async () => {
  const run1 = wireRun(1, 1_000, 1);
  const run2 = wireRun(2, 10_000, 10);
  const queries: TrajectoryQuery[] = [];
  // The second Run's first tool has no result yet when the tail is read.
  const pending = run2.map((record) => (record.id === "tool:c1-2" ? { ...record, kind: { ...record.kind, resultText: undefined } } : record));
  let reply: unknown = { records: pending, hasMoreBefore: true, firstSequence: 10, lastSequence: 14 };
  const log = new TrajectoryLog(async (query) => {
    queries.push(query);
    if (query.before === 10) return { records: run1, hasMoreBefore: false, firstSequence: 1, lastSequence: 5 };
    return reply;
  }, () => ({ kind: "failed", detail: "" }));
  await log.load();
  expect(log.records().length).toBe(7);
  await log.loadAll();
  expect(log.records().length).toBe(14);
  expect(log.hasMoreBefore()).toBe(false);
  // Catching up asks from just before the pending call and replaces what follows.
  reply = { records: run2.slice(1), lastSequence: 15 };
  await log.refresh();
  expect(queries.at(-1)).toEqual({ after: 10 });
  expect(log.records().length).toBe(14);
  const tool = log.records().find((record) => record.id === "tool:c1-2")!.kind;
  expect(tool.type === "tool" && tool.resultText).toBe("ok");
  // A new branch replaces everything.
  reply = { records: run1, branchChanged: true, firstSequence: 1, lastSequence: 5 };
  await log.refresh();
  expect(queries.at(-1)).toEqual({ after: 15 });
  expect(log.records().length).toBe(7);
});

test("a_failed_first_load_is_classified", async () => {
  const log = new TrajectoryLog(async () => { throw new Error("no history"); }, (error) => ({ kind: "unavailable", detail: String(error) }));
  await log.load();
  expect(log.failure()?.kind).toBe("unavailable");
  expect(log.loading()).toBe(false);
});

test("overlapping_loads_share_one_fetch_and_stale_pages_are_dropped", async () => {
  const run1 = wireRun(1, 1_000, 1);
  const run2 = wireRun(2, 10_000, 10);
  let tails = 0;
  let releaseEarlier: (() => void) | undefined;
  let tail: unknown = { records: run2, hasMoreBefore: true, firstSequence: 10, lastSequence: 14 };
  const log = new TrajectoryLog(async (query) => {
    if (query.before !== undefined) {
      await new Promise<void>((resolve) => { releaseEarlier = resolve; });
      return { records: run1, hasMoreBefore: false, firstSequence: 1, lastSequence: 5 };
    }
    if (query.after !== undefined) return { records: run1, branchChanged: true, firstSequence: 1, lastSequence: 5 };
    tails += 1;
    return tail;
  }, () => ({ kind: "failed", detail: "" }));
  await Promise.all([log.load(), log.refresh()]);
  expect(tails).toBe(1);
  expect(log.records().length).toBe(7);
  // An earlier page still in flight when the branch changes belongs to the old branch.
  const earlier = log.loadEarlier();
  await log.refresh();
  releaseEarlier!();
  await earlier;
  expect(log.records().map((record) => record.sequence)).toEqual(decoded(run1).map((record) => record.sequence));
  tail = undefined;
});

test("catching_up_anchors_on_a_loaded_branch_entry_not_the_number_before_the_pending_call", async () => {
  // Request contents take sequence numbers 7..10 off the branch: the entry before the pending call is 6.
  const run = wireRun(2, 10_000, 10).map((record) => (record.id === "u2" ? { ...record, sequence: 6 } : record));
  const pending = run.map((record) => (record.id === "tool:c1-2" ? { ...record, kind: { ...record.kind, resultText: undefined } } : record));
  const queries: TrajectoryQuery[] = [];
  let reply: unknown = { records: pending, firstSequence: 6, lastSequence: 14 };
  const log = new TrajectoryLog(async (query) => {
    queries.push(query);
    return reply;
  }, () => ({ kind: "failed", detail: "" }));
  await log.load();
  reply = { records: run.slice(1), lastSequence: 14 };
  await log.refresh();
  expect(queries.at(-1)).toEqual({ after: 6 });
  expect(log.records().length).toBe(7);
  // A pending call in the first loaded entry has no earlier cursor: the tail is read again.
  const first = new TrajectoryLog(async (query) => {
    queries.push(query);
    return { records: pending.slice(1), firstSequence: 11, lastSequence: 14 };
  }, () => ({ kind: "failed", detail: "" }));
  await first.load();
  await first.refresh();
  expect(queries.at(-1)).toEqual({});
});

test("run_headings_keep_their_key_as_earlier_records_load", () => {
  const records = decoded([...wireRun(1, 1_000, 1), ...wireRun(2, 10_000, 10)]);
  const run = (list: readonly TrajectoryRecord[]) => trajectoryRows(list, { collapsedRuns: new Set(), groupTools: true, openGroups: new Set() })
    .flatMap((row) => (row.type === "run" ? [row.key] : []));
  // Run 2 loaded from its middle first, then whole: its heading keeps one key, so a fold survives the earlier page.
  expect(run(records.slice(9))).toEqual(["run:run-2"]);
  expect(run(records)).toEqual(["run:run-1", "run:run-2"]);
  // Stretches between Runs are told apart by their first record.
  const between = decoded([{ id: "s1", sequence: 0, kind: { type: "setting", change: "model", detail: "m" } }, ...wireRun(1, 1_000, 1),
    { id: "s2", sequence: 9, kind: { type: "setting", change: "model", detail: "n" } }]);
  expect(run(between)).toEqual(["between:s1", "run:run-1", "between:s2"]);
  expect(runHeadingKey(between, between.at(-1)!)).toBe("between:s2");
});

test("a_call_left_without_a_result_by_an_earlier_run_is_not_pending", async () => {
  // Run 1 was interrupted with its first call unanswered; Run 2 ran to the end after it.
  const orphan = wireRun(1, 1_000, 1).map((record) => (record.id === "tool:c1-1" ? { ...record, kind: { ...record.kind, resultText: undefined } } : record));
  const queries: TrajectoryQuery[] = [];
  const log = new TrajectoryLog(async (query) => {
    queries.push(query);
    return query.after === undefined
      ? { records: [...orphan, ...wireRun(2, 10_000, 10)], firstSequence: 1, lastSequence: 14 }
      : { records: [], lastSequence: 14 };
  }, () => ({ kind: "failed", detail: "" }));
  await log.load();
  expect(log.pending(log.record("tool:c1-1")!)).toBe(false);
  // Catching up asks only for what follows the loaded records, not from the old call again.
  await log.refresh();
  expect(queries.at(-1)).toEqual({ after: 14 });
});
