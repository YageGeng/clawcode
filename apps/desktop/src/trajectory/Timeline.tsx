import { Canvas, cleanup, computed, effect, signal, Text, token, View } from "nsh";
import type { CanvasRenderingContext2D, CanvasSize } from "nsh";
import { t } from "nsh/i18n";

import { axisAt, panWindow, timeline, TOOL_ROWS, visibleBars, zoomWindow } from "../client/trajectory";
import type { PlacedBar, TrajectoryRecord, Window } from "../client/trajectory";
import { focusRing, type } from "../ui/tokens";
import { formatMs } from "./format";

/** Vertical layout of the overview, in points: Run labels, the three lanes, then the time ticks. */
const LAYOUT = {
  height: 112,
  label: 12,
  request: { top: 16, height: 14 },
  tool: { top: 36, row: 9 },
  event: { top: 68, height: 12 },
  ticks: 96,
  /** The y range of a placed bar's lane row. */
  rowsOf(bar: PlacedBar): { top: number; height: number } {
    if (bar.lane === "request") return LAYOUT.request;
    if (bar.lane === "event") return LAYOUT.event;
    return { top: LAYOUT.tool.top + bar.row * (LAYOUT.tool.row + 1), height: LAYOUT.tool.row };
  },
} as const;

/** A drag shorter than this many points is a click. */
const CLICK_SLOP = 3;

/** How long the pointer rests on a bar before its details float up. */
const HOVER_DELAY_MS = 500;

/**
 * The trajectory's time overview: requests, tool calls (in up to three rows) and events as bars over time, Runs as
 * separators. Click selects a record, dragging picks a time range, the wheel zooms (with Shift it pans), a double
 * click shows everything again, and a right click or Escape clears the range.
 */
export function Timeline(props: {
  records: readonly TrajectoryRecord[];
  even: boolean;
  /** Older records exist that are not loaded, so the left edge is not the branch's start. */
  truncated: boolean;
  /** Since when a request or tool has been running, drawn as a dashed bar to now. */
  runningSince?: number;
  selectedId?: string;
  range?: Window;
  onSelect: (recordId: string) => void;
  onRange: (range: Window | undefined) => void;
}) {
  const layout = computed(() => timeline(props.records, props.even));
  const width = signal(0);
  // A zoomed window, cleared when the extent changes shape (new records or another spacing).
  const zoom = signal<Window | undefined>(undefined);
  effect(() => {
    layout();
    zoom.set(undefined);
  });
  const now = signal(Date.now());
  effect(() => {
    if (props.runningSince === undefined) return;
    const timer = setInterval(() => now.set(Date.now()), 1000);
    return () => clearInterval(timer);
  });
  /** The extent, stretched to now while something runs (real time only). */
  const extent = computed<Window>(() => {
    const base = layout().extent;
    return props.runningSince !== undefined && !props.even ? { start: base.start, end: Math.max(base.end, now()) } : base;
  });
  const view = computed(() => zoom() ?? extent());
  const bars = computed(() => visibleBars(layout().bars, view(), width()));
  const drag = signal<{ from: number; to: number } | undefined>(undefined);
  const hover = signal<{ bar: PlacedBar; x: number } | undefined>(undefined);
  let hoverTimer: ReturnType<typeof setTimeout> | undefined;
  cleanup(() => clearTimeout(hoverTimer));

  /** The bar under a point, the nearest on its lane within a few points for thin ones. */
  const barAt = (x: number, y: number): PlacedBar | undefined => {
    let best: PlacedBar | undefined;
    let distance = 4;
    for (const bar of bars()) {
      const rows = LAYOUT.rowsOf(bar);
      if (y < rows.top - 2 || y > rows.top + rows.height + 2) continue;
      const gap = x < bar.x ? bar.x - x : x > bar.x + Math.max(bar.w, 1) ? x - bar.x - Math.max(bar.w, 1) : 0;
      if (gap <= distance) {
        best = bar;
        distance = gap;
      }
    }
    return best;
  };

  /** A dragged span on the axis as a time range: itself in real time, the records it covers when spaced evenly. */
  const rangeOf = (from: number, to: number): Window | undefined => {
    const span = { start: axisAt(Math.min(from, to), width(), view()), end: axisAt(Math.max(from, to), width(), view()) };
    if (!props.even) return span;
    const covered = layout().bars.filter((bar) => bar.end >= span.start && bar.start <= span.end)
      .flatMap((bar) => props.records.find((record) => record.id === bar.id) ?? []);
    const starts = covered.flatMap((record) => (record.startedAtMs === undefined ? [] : [record.startedAtMs]));
    const ends = covered.flatMap((record) => (record.endedAtMs ?? record.startedAtMs ?? []));
    return starts.length ? { start: Math.min(...starts), end: Math.max(...ends) } : undefined;
  };

  /** Draws lanes, Run separators, bars, the running bar, the selection, the dragged span and the ticks. */
  const draw = (cx: CanvasRenderingContext2D, size: CanvasSize) => {
    const window = view();
    const scale = size.width / Math.max(1e-9, window.end - window.start);
    const xOf = (value: number) => (value - window.start) * scale;
    const muted = String(token("muted"));
    const mutedText = String(token("muted-foreground"));
    // The theme's chart colors are greys; the semantic hues keep the three lanes apart in both appearances.
    const primary = String(token("info"));
    const tool = String(token("success"));
    const event = String(token("warning"));
    const danger = String(token("destructive"));
    const border = String(token("border"));
    // Lane backgrounds with soft rounded ends matching the app's geometry.
    cx.fillStyle = muted;
    cx.globalAlpha = 0.4;
    for (const lane of [
      { top: LAYOUT.request.top, height: LAYOUT.request.height },
      { top: LAYOUT.tool.top, height: TOOL_ROWS * (LAYOUT.tool.row + 1) - 1 },
      { top: LAYOUT.event.top, height: LAYOUT.event.height },
    ]) {
      cx.beginPath();
      cx.roundRect(0, lane.top, size.width, lane.height, 3);
      cx.fill();
    }
    cx.globalAlpha = 1;
    cx.font = "500 10px sans-serif";
    cx.textBaseline = "top";
    // Run separators with their labels; omit the vertical rule when flush against the left edge.
    for (const run of layout().runs) {
      const x = xOf(run.at);
      if (x < -1 || x > size.width + 1) continue;
      if (x > 2) {
        cx.fillStyle = border;
        cx.fillRect(x, LAYOUT.label, 1, LAYOUT.ticks - LAYOUT.label - 2);
      }
      cx.fillStyle = mutedText;
      cx.fillText(t("trajectory.run", { n: run.index }), Math.max(2, x + 4), 0);
    }
    for (const bar of bars()) {
      const rows = LAYOUT.rowsOf(bar);
      const w = Math.max(bar.w, bar.lane === "event" ? 2.5 : 1.5);
      const color = bar.failed ? danger : bar.lane === "request" ? primary : bar.lane === "tool" ? tool : event;
      const barRadius = bar.lane === "event" ? 1.5 : 2.5;
      if (bar.lane === "request" && bar.firstToken !== undefined) {
        // Clip the two-tone request bar into one rounded pill: lighter while waiting for the first token.
        const split = Math.min(Math.max(xOf(bar.firstToken), bar.x), bar.x + w);
        cx.save();
        cx.beginPath();
        cx.roundRect(bar.x, rows.top, w, rows.height, barRadius);
        cx.clip();
        cx.fillStyle = color;
        cx.globalAlpha = 0.35;
        cx.fillRect(bar.x, rows.top, split - bar.x, rows.height);
        cx.globalAlpha = 1;
        cx.fillRect(split, rows.top, bar.x + w - split, rows.height);
        cx.restore();
      } else {
        cx.fillStyle = color;
        cx.beginPath();
        cx.roundRect(bar.x, rows.top, w, rows.height, barRadius);
        cx.fill();
      }
      if (bar.count > 1 && w > 14) {
        cx.fillStyle = String(token("background"));
        cx.fillText(String(bar.count), bar.x + 2.5, rows.top);
      }
      if (bar.id === props.selectedId) {
        cx.strokeStyle = String(token("foreground"));
        cx.lineWidth = 1.5;
        cx.beginPath();
        cx.roundRect(bar.x - 1.5, rows.top - 1.5, w + 3, rows.height + 3, barRadius + 1);
        cx.stroke();
      }
    }
    if (props.runningSince !== undefined && !props.even) {
      // The work in progress: a dashed bar from its start to now.
      const x = xOf(props.runningSince);
      cx.strokeStyle = primary;
      cx.lineWidth = 2;
      cx.setLineDash([4, 3]);
      cx.beginPath();
      cx.moveTo(x, LAYOUT.request.top + LAYOUT.request.height / 2);
      cx.lineTo(xOf(now()), LAYOUT.request.top + LAYOUT.request.height / 2);
      cx.stroke();
      cx.setLineDash([]);
    }
    const shown = drag() ?? (props.range && !props.even ? { from: xOf(props.range.start), to: xOf(props.range.end) } : undefined);
    if (shown) {
      cx.fillStyle = primary;
      cx.globalAlpha = 0.12;
      cx.beginPath();
      cx.roundRect(Math.min(shown.from, shown.to), LAYOUT.label, Math.abs(shown.to - shown.from), LAYOUT.ticks - LAYOUT.label - 2, 3);
      cx.fill();
      cx.globalAlpha = 1;
    }
    // Ticks: five labels across (0% through 100%), offsets from the first loaded record in real time.
    if (!props.even) {
      cx.fillStyle = mutedText;
      const origin = layout().extent.start;
      for (let i = 0; i <= 4; i++) {
        const x = (size.width * i) / 4;
        const label = `+${formatMs(axisAt(x, size.width, window) - origin)}`;
        if (i === 4) {
          cx.textAlign = "right";
          cx.fillText(label, size.width - 2, LAYOUT.ticks);
          cx.textAlign = "left";
        } else {
          cx.fillText(label, x + 2, LAYOUT.ticks);
        }
      }
    }
    if (props.truncated) {
      cx.fillStyle = mutedText;
      cx.textAlign = "right";
      cx.fillText(t("trajectory.earlierNotLoaded"), size.width - 2, 0);
      cx.textAlign = "left";
    }
  };

  /** Shows a bar's details after the pointer rested on it. */
  const rest = (x: number, y: number) => {
    clearTimeout(hoverTimer);
    const bar = barAt(x, y);
    if (!bar) return hover.set(undefined);
    if (hover()?.bar.id === bar.id) return;
    hover.set(undefined);
    hoverTimer = setTimeout(() => hover.set({ bar, x }), HOVER_DELAY_MS);
  };

  /** What the hover card says about a bar: when it started, how long, and a request's token timing. */
  const hoverLines = (bar: PlacedBar): string[] => {
    if (bar.count > 1) return [t("trajectory.hover.merged", { count: bar.count })];
    const record = props.records.find((item) => item.id === bar.id);
    if (!record) return [];
    const origin = props.records.find((item) => item.startedAtMs !== undefined)?.startedAtMs ?? 0;
    const lines = [record.startedAtMs === undefined ? "" : t("trajectory.hover.started", { at: `+${formatMs(record.startedAtMs - origin)}` })];
    const kind = record.kind;
    if (kind.type === "assistant" && kind.lastTokenAtMs >= kind.requestedAtMs) {
      lines.push(t("trajectory.hover.took", { duration: formatMs(kind.lastTokenAtMs - kind.requestedAtMs) }));
      lines.push(t("trajectory.hover.firstToken", { duration: formatMs(kind.firstTokenAtMs - kind.requestedAtMs) }));
      const generation = kind.lastTokenAtMs - kind.firstTokenAtMs;
      lines.push(t("trajectory.hover.generation", { duration: formatMs(generation) }));
      if (generation > 0) lines.push(t("trajectory.hover.throughput", { rate: (kind.usage.output / (generation / 1000)).toFixed(1) }));
    } else if (record.startedAtMs !== undefined && record.endedAtMs !== undefined) {
      lines.push(t("trajectory.hover.took", { duration: formatMs(record.endedAtMs - record.startedAtMs) }));
    }
    return lines.filter(Boolean);
  };

  return (
    <View class="border-b border-border bg-muted/15 px-3 py-2" onLayout={({ bounds }) => width.set(Math.max(0, bounds.size.width - 24))}>
      <View
        accessibilityLabel={t("trajectory.overview")}
        focusable
        captureKeys={["left", "right", "escape"]}
        class={`rounded-(--radius) ${focusRing}`}
        style={{ height: LAYOUT.height }}
        onPressDown={(e) => drag.set({ from: e.position.x, to: e.position.x })}
        onMouseMove={(e) => {
          const current = drag();
          if (current && e.pressedButton !== null) return drag.set({ ...current, to: e.position.x });
          rest(e.position.x, e.position.y);
        }}
        onMouseLeave={() => {
          clearTimeout(hoverTimer);
          hover.set(undefined);
        }}
        onPressUp={(e) => {
          const current = drag();
          drag.set(undefined);
          if (!current) return;
          if (Math.abs(e.position.x - current.from) > CLICK_SLOP) return props.onRange(rangeOf(current.from, e.position.x));
          if (e.clickCount >= 2) return zoom.set(undefined);
          const bar = barAt(e.position.x, e.position.y);
          if (bar) props.onSelect(bar.id);
        }}
        onContextMenu={() => props.onRange(undefined)}
        onWheel={(e) => {
          const step = e.deltaMode === "lines" ? e.delta.y * 0.1 : e.delta.y / 300;
          if (e.modifiers.shift) return zoom.set(panWindow(view(), step, extent()));
          zoom.set(zoomWindow(view(), e.position.x / Math.max(1, width()), Math.exp(step), extent()));
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape") return props.onRange(undefined);
          if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
          // Arrow keys move the selection through the drawn bars in time order.
          const ordered = [...layout().bars].sort((a, b) => a.start - b.start);
          const at = ordered.findIndex((bar) => bar.id === props.selectedId);
          const next = ordered[at < 0 ? 0 : Math.min(ordered.length - 1, Math.max(0, at + (e.key === "ArrowLeft" ? -1 : 1)))];
          if (next) props.onSelect(next.id);
        }}
      >
        <Canvas class="absolute inset-0" draw={draw} />
        <If when={hover()}>
          {(shown) => {
            const lane = () => LAYOUT.rowsOf(shown().bar);
            // Place the floating details just below the hovered bar, or above when hovering the bottom event lane.
            const top = () => (shown().bar.lane === "event" ? Math.max(0, lane().top - 52) : lane().top + lane().height + 4);
            return (
              <View class="absolute z-10 gap-0.5 rounded-(--radius) border border-border bg-popover px-2.5 py-1.5 shadow-lg"
                style={{ left: Math.min(Math.max(0, shown().x + 8), Math.max(0, width() - 188)), top: top() }}>
                <Each of={hoverLines(shown().bar)}>
                  {(line) => <Text class={`tabular-nums ${type.meta} text-popover-foreground`}>{line()}</Text>}
                </Each>
              </View>
            );
          }}
        </If>
      </View>
      {/* The lanes have no room for names, so one legend says what each color is, lane by lane. */}
      <View accessibilityElementsHidden class="mt-1.5 flex-row flex-wrap items-center gap-x-4 gap-y-1">
        <View class="flex-row items-center gap-1.5">
          <View class="flex-row overflow-hidden rounded-xs">
            <View class="h-2 w-1.5 bg-info/35" />
            <View class="h-2 w-1.5 bg-info" />
          </View>
          <Text class={`${type.meta} text-muted-foreground`}>{t("trajectory.legend.request")}</Text>
        </View>
        <Each of={[["bg-success", "tool"], ["bg-warning", "event"], ["bg-destructive", "failed"]] as const}>
          {(entry) => (
            <View class="flex-row items-center gap-1.5">
              <View class={`size-2 rounded-xs ${entry()[0]}`} />
              <Text class={`${type.meta} text-muted-foreground`}>{t(`trajectory.legend.${entry()[1]}`)}</Text>
            </View>
          )}
        </Each>
      </View>
    </View>
  );
}
