//! Timeline geometry: which records draw where, lane packing, zoom and pan,
//! and pixel mapping. Pure arithmetic, so the canvas only paints.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use protocol::{TrajectoryRecord, TrajectoryRecordKind};
use typed_builder::TypedBuilder;

use super::{InFlight, TimeInterval};

/// Sub-rows of the tool lane; later overlapping calls merge into the last.
const TOOL_ROWS: u8 = 3;

/// Bars narrower than this merge with their thin neighbours, in pixels.
const MERGE_WIDTH: f32 = 1.0;

/// How far from a bar a click still hits it, in pixels.
const HIT_SLOP: f32 = 2.0;

/// The narrowest view, as a share of the domain.
const MIN_ZOOM_SHARE: f64 = 1e-4;

/// A non-negative axis value as a whole number, saturating; float to
/// integer casts saturate in Rust, and the value is clamped at zero first.
#[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
pub fn whole(value: f64) -> u64 {
    value.max(0.0) as u64
}

/// How time maps to the horizontal axis.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DurationMode {
    /// Real elapsed time.
    Actual,
    /// One equal unit per record, in order; the default, since real time
    /// crowds short records into slivers beside long ones.
    #[default]
    Equal,
}

/// A horizontal lane of the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Lane {
    /// Model requests.
    Request,
    /// Tool calls, by sub-row 0–2.
    Tool(u8),
    /// User messages, prompt or tool changes, compaction, setting changes.
    Event,
}

/// Height of the whole timeline, in rem.
pub const TIMELINE_REMS: f32 = 6.0;

/// Where the time-tick labels start, in rem from the top.
pub const TICKS_TOP_REMS: f32 = 4.75;

impl Lane {
    /// Every lane, top to bottom.
    pub const ALL: [Self; 5] = [
        Self::Request,
        Self::Tool(0),
        Self::Tool(1),
        Self::Tool(2),
        Self::Event,
    ];

    /// The lane's top edge and height, in rem from the timeline's top; the
    /// Run labels sit above the first lane.
    pub fn band_rems(self) -> (f32, f32) {
        match self {
            Self::Request => (0.875, 1.0),
            Self::Tool(row) => {
                (2.0 + 0.625 * f32::from(row.min(TOOL_ROWS - 1)), 0.5)
            }
            Self::Event => (3.875, 0.75),
        }
    }

    /// The lane at `y` rem from the timeline's top, if any.
    pub fn at_rems(y: f32) -> Option<Self> {
        Self::ALL.into_iter().find(|lane| {
            let (top, height) = lane.band_rems();
            (top..top + height).contains(&y)
        })
    }
}

/// One drawable item in timeline units: milliseconds, or record units in
/// equal mode.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct Span {
    /// The record it draws; the first one when merged.
    pub record_id: String,
    /// Where it draws.
    pub lane: Lane,
    /// Start.
    pub start: f64,
    /// First token of a request.
    #[builder(default)]
    pub first_token: Option<f64>,
    /// End; `None` for an event marker.
    #[builder(default)]
    pub end: Option<f64>,
    /// It failed or was cancelled.
    #[builder(default)]
    pub failed: bool,
    /// It is still running.
    #[builder(default)]
    pub running: bool,
    /// Records it stands for.
    #[builder(default = 1)]
    pub merged: u32,
}

impl Span {
    /// The end, or the start for a marker.
    fn until(&self) -> f64 {
        self.end.unwrap_or(self.start)
    }

    /// Takes `other` into this span.
    fn absorb(&mut self, other: &Self) {
        self.end = Some(self.until().max(other.until()));
        self.failed |= other.failed;
        self.running |= other.running;
        self.merged += other.merged;
    }
}

/// One bar in pixels.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct Bar {
    /// The record it draws; the first one when merged.
    pub record_id: String,
    /// Where it draws.
    pub lane: Lane,
    /// Left edge.
    pub x: f32,
    /// First-token position of a request.
    #[builder(default)]
    pub first_token_x: Option<f32>,
    /// Width; zero for an event marker.
    pub w: f32,
    /// It failed or was cancelled.
    pub failed: bool,
    /// It is still running.
    pub running: bool,
    /// Records it stands for.
    pub merged: u32,
}

/// The visible part of the time axis.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    /// Visible range in timeline units.
    pub visible: Range<f64>,
}

impl Viewport {
    /// Shows the whole domain.
    pub fn full(domain: &Range<f64>) -> Self {
        Self {
            visible: domain.clone(),
        }
    }

    /// Width of the visible range.
    fn width(&self) -> f64 {
        self.visible.end - self.visible.start
    }

    /// Zooms by `factor` (above 1 zooms in) keeping the time at `anchor`, a
    /// 0–1 position across the view, in place, within `domain`.
    pub fn zoom(&mut self, factor: f64, anchor: f64, domain: &Range<f64>) {
        let full = domain.end - domain.start;
        let width = (self.width() / factor).clamp(full * MIN_ZOOM_SHARE, full);
        let at = self.visible.start + anchor * self.width();
        self.place(at - anchor * width, width, domain);
    }

    /// Moves the view by `fraction` of its width, within `domain`.
    pub fn pan(&mut self, fraction: f64, domain: &Range<f64>) {
        let width = self.width();
        self.place(self.visible.start + fraction * width, width, domain);
    }

    /// Shows `width` from `start`, shifted back inside `domain`.
    fn place(&mut self, start: f64, width: f64, domain: &Range<f64>) {
        let start =
            start.clamp(domain.start, (domain.end - width).max(domain.start));
        self.visible = start..start + width;
    }

    /// The pixel of time `t` in a view `width` pixels wide.
    pub fn x_of(&self, t: f64, width: f32) -> f32 {
        let share = (t - self.visible.start) / self.width();
        (share * f64::from(width)) as f32
    }

    /// The time at pixel `x` in a view `width` pixels wide.
    pub fn t_of(&self, x: f32, width: f32) -> f64 {
        self.visible.start + f64::from(x) / f64::from(width) * self.width()
    }
}

/// The spans of the loaded records and the axis they lie on.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct TimelineLayout {
    #[builder(default)]
    spans: Vec<Span>,
    /// Where each Run starts: its number and time.
    #[builder(default)]
    run_starts: Vec<(u32, f64)>,
    #[builder(default = 0.0..1.0)]
    domain: Range<f64>,
    /// Equal mode: the real start and end of each unit.
    #[builder(default)]
    units: Vec<(u64, u64)>,
}

impl TimelineLayout {
    /// Lays out `records` and the `in_flight` work, running ones up to `now_ms`.
    pub fn new(
        records: &[TrajectoryRecord],
        in_flight: &[InFlight],
        now_ms: u64,
        mode: DurationMode,
    ) -> Self {
        let now = now_ms as f64;
        // What executes is known from the session, not from a missing result:
        // a result that never reached the history must not run forever.
        let executing: HashMap<String, u64> = in_flight
            .iter()
            .filter_map(|work| Some((work.record_id()?, work.started_at_ms())))
            .collect();
        let loaded: HashSet<&str> =
            records.iter().map(|record| record.id.as_str()).collect();
        let mut layout = Self::builder().build();
        let mut tool_rows: [Option<usize>; TOOL_ROWS as usize] = [None; 3];
        let mut last_run = None;
        let items = records
            .iter()
            .filter_map(|record| {
                Self::span_of(record, now, executing.contains_key(&record.id))
            })
            // A loaded call draws as its record.
            .chain(
                in_flight
                    .iter()
                    .enumerate()
                    .filter(|(_, work)| {
                        work.record_id()
                            .is_none_or(|id| !loaded.contains(id.as_str()))
                    })
                    .map(|(n, work)| {
                        let (record_id, lane) = match work {
                            InFlight::Request { .. } => {
                                (format!("in-flight:{n}"), Lane::Request)
                            }
                            InFlight::Tool { .. } => {
                                (format!("in-flight:{n}"), Lane::Tool(0))
                            }
                        };
                        (
                            None,
                            Span::builder()
                                .record_id(record_id)
                                .lane(lane)
                                .start(work.started_at_ms() as f64)
                                .end(Some(now))
                                .running(true)
                                .build(),
                        )
                    }),
            );
        for (run, mut span) in items {
            if mode == DurationMode::Equal {
                let unit = layout.units.len() as f64;
                layout.units.push((whole(span.start), whole(span.until())));
                let length = span.until() - span.start;
                span.first_token = span.first_token.map(|first| {
                    unit + if length > 0.0 {
                        (first - span.start) / length
                    } else {
                        0.0
                    }
                });
                span.start = unit;
                span.end = span.end.map(|_| unit + 1.0);
            }
            if let Some(index) = run
                && last_run != Some(index)
            {
                layout.run_starts.push((index, span.start));
                last_run = Some(index);
            }
            if matches!(span.lane, Lane::Tool(_)) {
                layout.pack_tool(span, &mut tool_rows);
            } else {
                layout.spans.push(span);
            }
        }
        layout.domain = match mode {
            DurationMode::Equal => 0.0..(layout.units.len().max(1) as f64),
            DurationMode::Actual => {
                let start = layout
                    .spans
                    .iter()
                    .map(|span| span.start)
                    .fold(f64::INFINITY, f64::min);
                let end = layout
                    .spans
                    .iter()
                    .map(Span::until)
                    .fold(f64::NEG_INFINITY, f64::max);
                if start.is_finite() && end > start {
                    start..end
                } else if start.is_finite() {
                    start..start + 1.0
                } else {
                    0.0..1.0
                }
            }
        };
        layout
    }

    /// The span a record draws, with its Run number; an `executing` call
    /// runs up to `now`.
    fn span_of(
        record: &TrajectoryRecord,
        now: f64,
        executing: bool,
    ) -> Option<(Option<u32>, Span)> {
        let start = record.started_at_ms?.get() as f64;
        let end = record.ended_at_ms.map(|at| at.get() as f64);
        let (lane, first_token, end, failed, running) = match &record.kind {
            TrajectoryRecordKind::Assistant(assistant) => (
                Lane::Request,
                Some(assistant.first_token_at_ms.get() as f64),
                Some(assistant.last_token_at_ms.get() as f64),
                assistant.error.is_some() || assistant.cancelled,
                false,
            ),
            TrajectoryRecordKind::Tool(tool) => (
                Lane::Tool(0),
                None,
                Some(if executing { now } else { end.unwrap_or(start) }),
                tool.is_error,
                executing,
            ),
            TrajectoryRecordKind::Request(request)
                if request.prompt_changed
                    || !request.tools_added.is_empty()
                    || !request.tools_removed.is_empty() =>
            {
                (Lane::Event, None, None, false, false)
            }
            TrajectoryRecordKind::User(_)
            | TrajectoryRecordKind::Command(_)
            | TrajectoryRecordKind::Compaction { .. }
            | TrajectoryRecordKind::BranchSummary { .. }
            | TrajectoryRecordKind::Setting(_) => {
                (Lane::Event, None, None, false, false)
            }
            _ => return None,
        };
        Some((
            record.run.as_ref().map(|run| run.index),
            Span::builder()
                .record_id(record.id.clone())
                .lane(lane)
                .start(start)
                .first_token(first_token)
                .end(end)
                .failed(failed)
                .running(running)
                .build(),
        ))
    }

    /// Puts a tool span on the first free sub-row, or merges it into the
    /// last sub-row's span when all are busy.
    fn pack_tool(
        &mut self,
        mut span: Span,
        rows: &mut [Option<usize>; TOOL_ROWS as usize],
    ) {
        let free = rows.iter().position(|row| {
            row.and_then(|index| self.spans.get(index))
                .is_none_or(|occupant| occupant.until() <= span.start)
        });
        match free {
            Some(row) => {
                span.lane =
                    Lane::Tool(u8::try_from(row).unwrap_or(TOOL_ROWS - 1));
                if let Some(slot) = rows.get_mut(row) {
                    *slot = Some(self.spans.len());
                }
                self.spans.push(span);
            }
            None => {
                if let Some(occupant) = rows
                    .last()
                    .copied()
                    .flatten()
                    .and_then(|index| self.spans.get_mut(index))
                {
                    occupant.absorb(&span);
                }
            }
        }
    }

    /// Where `interval` lies on the axis: its real times in actual mode; in
    /// equal mode, the units of the records it overlaps, the ones its filter
    /// keeps. `None` when it overlaps no unit.
    ///
    /// Units follow record order, whose ends need not grow (a long request
    /// can end after the records that follow it), so this scans rather than
    /// searches; it runs once a frame, only while an interval is chosen.
    pub fn axis_range(&self, interval: &TimeInterval) -> Option<Range<f64>> {
        if self.units.is_empty() {
            return Some(interval.start_ms as f64..interval.end_ms as f64);
        }
        let mut overlapping =
            self.units.iter().enumerate().filter(|(_, unit)| {
                unit.0 <= interval.end_ms && unit.1 >= interval.start_ms
            });
        let first = overlapping.next()?.0;
        let last = overlapping.next_back().map_or(first, |(index, _)| index);
        Some(first as f64..(last + 1) as f64)
    }

    /// Whether some span is still running, so the layout grows with time.
    pub fn running(&self) -> bool {
        self.spans.iter().any(|span| span.running)
    }

    /// The spans, in record order per lane.
    pub fn spans(&self) -> &[Span] {
        &self.spans
    }

    /// Where each Run starts: its number and time.
    pub fn run_starts(&self) -> &[(u32, f64)] {
        &self.run_starts
    }

    /// The full axis range.
    pub fn domain(&self) -> &Range<f64> {
        &self.domain
    }

    /// The bars inside `viewport` drawn `width` pixels wide; thin neighbours
    /// on one lane merge into one bar.
    pub fn bars(&self, viewport: &Viewport, width: f32) -> Vec<Bar> {
        let mut lanes: BTreeMap<Lane, Vec<Bar>> = BTreeMap::new();
        for span in &self.spans {
            if span.start > viewport.visible.end
                || span.until() < viewport.visible.start
            {
                continue;
            }
            let x = viewport.x_of(span.start, width);
            let bar = Bar::builder()
                .record_id(span.record_id.clone())
                .lane(span.lane)
                .x(x)
                .first_token_x(
                    span.first_token.map(|first| viewport.x_of(first, width)),
                )
                .w(span.end.map_or(0.0, |end| {
                    (viewport.x_of(end, width) - x).max(0.0)
                }))
                .failed(span.failed)
                .running(span.running)
                .merged(span.merged)
                .build();
            let lane = lanes.entry(span.lane).or_default();
            match lane.last_mut() {
                Some(last)
                    if last.w < MERGE_WIDTH
                        && bar.w < MERGE_WIDTH
                        && bar.x - (last.x + last.w) < MERGE_WIDTH =>
                {
                    last.w = (bar.x + bar.w).max(last.x + last.w) - last.x;
                    last.first_token_x = None;
                    last.failed |= bar.failed;
                    last.running |= bar.running;
                    last.merged += bar.merged;
                }
                _ => lane.push(bar),
            }
        }
        lanes.into_values().flatten().collect()
    }

    /// The record drawn at pixel `x` on `lane`, the nearest within a small slop.
    pub fn hit(
        &self,
        viewport: &Viewport,
        width: f32,
        x: f32,
        lane: Lane,
    ) -> Option<&str> {
        self.spans
            .iter()
            .filter(|span| span.lane == lane)
            .filter_map(|span| {
                let left = viewport.x_of(span.start, width);
                let right = viewport.x_of(span.until(), width);
                let distance = if x < left {
                    left - x
                } else if x > right {
                    x - right
                } else {
                    0.0
                };
                (distance <= HIT_SLOP).then_some((distance, span))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, span)| span.record_id.as_str())
    }

    /// The real time range between two pixels; in equal mode, the real span
    /// of the records whose units the range covers.
    pub fn interval(
        &self,
        viewport: &Viewport,
        width: f32,
        from_x: f32,
        to_x: f32,
    ) -> TimeInterval {
        let from = viewport.t_of(from_x.min(to_x), width);
        let to = viewport.t_of(from_x.max(to_x), width);
        if self.units.is_empty() {
            return TimeInterval {
                start_ms: whole(from),
                end_ms: whole(to),
            };
        }
        let units = self.units.len();
        let index = |unit: f64| usize::try_from(whole(unit)).unwrap_or(units);
        let first = index(from.floor()).min(units - 1);
        let last = index(to.ceil()).clamp(first + 1, units);
        let covered = self.units.get(first..last).unwrap_or_default();
        TimeInterval {
            start_ms: covered
                .iter()
                .map(|unit| unit.0)
                .min()
                .unwrap_or_default(),
            end_ms: covered.iter().map(|unit| unit.1).max().unwrap_or_default(),
        }
    }
}
