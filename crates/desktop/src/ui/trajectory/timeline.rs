//! The time overview above the ledger (M4a spec §6): a canvas painting the
//! lanes the geometry in [`crate::trajectory::timeline`] lays out, and the
//! pointer and keyboard interactions on it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, Bounds, Context, FocusHandle, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, ScrollEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _,
    Window, canvas, div, fill, point, prelude::FluentBuilder as _, px, rems,
    size,
};
use protocol::TrajectoryRecordKind;
use typed_builder::TypedBuilder;

use super::table::Hue;
use super::{TimelineVisible, TrajectoryView};
use crate::trajectory::timeline::{
    Bar, DurationMode, Lane, TICKS_TOP_REMS, TIMELINE_REMS, TimelineLayout,
    Viewport, whole,
};
use crate::trajectory::{InFlight, TimeInterval};
use crate::ui::Elapsed;

/// How long the pointer rests on a bar before its details float up.
const HOVER_DELAY: Duration = Duration::from_millis(500);

/// A press that moves less than this is a click, in pixels.
const CLICK_SLOP: f32 = 3.0;

/// Space a run label needs before the next one may start, in rems; fits
/// "Run 100" and "第 100 轮" at the extra-small size.
const RUN_LABEL_REMS: f32 = 4.5;

/// Zoom per pixel of wheel travel.
const ZOOM_PER_PIXEL: f64 = 0.004;

/// Tick labels across the visible range.
const TICKS: usize = 4;

/// What a cached layout was built from.
#[derive(Debug, Clone, PartialEq)]
struct LayoutKey {
    revision: u64,
    mode: DurationMode,
    in_flight: Vec<InFlight>,
}

/// The time overview's state within one trajectory view.
#[derive(TypedBuilder)]
pub(super) struct TimelineState {
    /// Real time or one unit per record.
    #[builder(default)]
    mode: DurationMode,
    /// The visible part of the axis.
    #[builder(default = Viewport::full(&(0.0..1.0)))]
    viewport: Viewport,
    /// Whether the view is zoomed in; otherwise it follows the records.
    #[builder(default)]
    zoomed: bool,
    /// The layout drawn last and what it was built from.
    #[builder(default, setter(skip))]
    layout_cache: Option<(LayoutKey, Rc<TimelineLayout>)>,
    /// The overview's bounds on the last frame.
    #[builder(default)]
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The rem size on the last frame, to find lanes under the pointer.
    #[builder(default = px(16.))]
    rem: Pixels,
    /// A press being dragged.
    #[builder(default)]
    drag: Option<TimelineDrag>,
    /// The bar the pointer rests on.
    #[builder(default)]
    hover: Option<TimelineHover>,
    /// Lets the overview take arrow keys and Escape.
    focus: FocusHandle,
}

impl TimelineState {
    /// An unzoomed overview, one equal unit per record.
    pub(super) fn new(focus: FocusHandle) -> Self {
        Self::builder().focus(focus).build()
    }

    /// Real time or one unit per record.
    pub(super) fn mode(&self) -> DurationMode {
        self.mode
    }

    /// The layout drawn last.
    fn layout(&self) -> Option<Rc<TimelineLayout>> {
        self.layout_cache
            .as_ref()
            .map(|(_, layout)| Rc::clone(layout))
    }
}

/// A press on the timeline being dragged.
#[derive(Debug, Clone, Copy)]
pub(super) struct TimelineDrag {
    /// Left drags pick an interval; right drags pan.
    button: MouseButton,
    /// Where the press started, in pixels from the timeline's left.
    from: f32,
    /// Where the pointer is now.
    to: f32,
}

/// The bar under a resting pointer.
#[derive(TypedBuilder)]
pub(super) struct TimelineHover {
    /// The record under the pointer.
    record_id: String,
    /// The pointer, in pixels from the timeline's top left.
    at: Point<f32>,
    /// Whether the delay passed and the details show.
    #[builder(default)]
    shown: bool,
    /// Shows the details once the delay passes.
    _timer: Task<()>,
}

impl TrajectoryView {
    /// The overview, or nothing while it is turned off.
    pub(super) fn render_timeline(
        &mut self,
        window: &Window,
        cx: &mut Context<'_, Self>,
    ) -> Option<AnyElement> {
        if !TimelineVisible::get(cx) || self.model.records().is_empty() {
            return None;
        }
        let layout = self.timeline_layout();
        // Unzoomed, the view follows the loaded records as they grow.
        if self.timeline.zoomed {
            self.timeline.viewport.pan(0.0, layout.domain());
        } else {
            self.timeline.viewport = Viewport::full(layout.domain());
        }
        let width = f32::from(self.timeline.bounds.get().size.width);
        let rem = window.rem_size();
        self.timeline.rem = rem;
        // One set of bars per frame, shared by the canvas and the labels.
        let bars: Rc<[Bar]> =
            layout.bars(&self.timeline.viewport, width).into();
        let labels = self.render_labels(&layout, &bars, width, rem, cx);
        let paint = self.painter(Rc::clone(&layout), Rc::clone(&bars), cx);
        let bounds = Rc::clone(&self.timeline.bounds);
        let view = cx.entity().downgrade();
        Some(
            div()
                .id("trajectory-timeline")
                // Moves only arrive over the overview, so leaving it must
                // drop the hover details, or they stay up.
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    if !hovered && this.timeline.hover.take().is_some() {
                        cx.notify();
                    }
                }))
                .test_support()
                .track_focus(&self.timeline.focus)
                .relative()
                .flex_none()
                .h(rems(TIMELINE_REMS))
                .w_full()
                .overflow_hidden()
                .border_b_1()
                .border_color(cx.theme().border)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(Self::on_timeline_down),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(Self::on_timeline_down),
                )
                .on_mouse_move(cx.listener(Self::on_timeline_move))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(Self::on_timeline_up),
                )
                .on_mouse_up(
                    MouseButton::Right,
                    cx.listener(Self::on_timeline_up),
                )
                .on_scroll(cx.listener(Self::on_timeline_scroll))
                .on_key_down(cx.listener(Self::on_timeline_key))
                .child(
                    canvas(
                        move |area, _, cx| {
                            // Labels need the width, which the first frame learns
                            // here; the shell draws this view cached, so notify it.
                            if bounds.get().size.width != area.size.width {
                                cx.defer(move |cx| {
                                    let _ =
                                        view.update(cx, |_, cx| cx.notify());
                                });
                            }
                            bounds.set(area);
                        },
                        move |area, (), window, cx| paint(area, window, cx),
                    )
                    .absolute()
                    .size_full(),
                )
                .children(labels)
                .into_any_element(),
        )
    }

    /// The paint closure: lanes, Run separators, bars, the interval, and a
    /// drag in progress. Every coordinate comes from the layout and viewport.
    fn painter(
        &self,
        layout: Rc<TimelineLayout>,
        bars: Rc<[Bar]>,
        cx: &Context<'_, Self>,
    ) -> impl FnOnce(Bounds<Pixels>, &mut Window, &mut gpui_kit::App) + 'static
    {
        let theme = cx.theme();
        let colors = BarColors::builder()
            .request(Hue::Request.color(theme))
            .tool(Hue::Tool.color(theme))
            .event(theme.muted_foreground)
            .danger(theme.danger)
            .separator(theme.border)
            .interval(theme.primary.opacity(0.12))
            .selected(theme.ring)
            .build();
        let viewport = self.timeline.viewport.clone();
        let interval = self.model.interval();
        let drag = self
            .timeline
            .drag
            .filter(|drag| drag.button == MouseButton::Left);
        let selected =
            self.model.selected_record().map(|record| record.id.clone());
        move |area, window, _| {
            let width = f32::from(area.size.width);
            let rem = window.rem_size();
            let origin = area.origin;
            // Pixel-exact canvas math; 1 px is the hairline width.
            let rect = |x: f32, top: f32, w: f32, h: f32| {
                Bounds::new(
                    point(origin.x + px(x), origin.y + rem * top),
                    size(px(w.max(1.0)), rem * h),
                )
            };
            for (_, start) in layout.run_starts() {
                let x = viewport.x_of(*start, width);
                window.paint_quad(fill(
                    rect(x, 0.0, 1.0, TICKS_TOP_REMS),
                    colors.separator,
                ));
            }
            let span = |range: (f32, f32), color: Hsla, window: &mut Window| {
                let (left, right) =
                    (range.0.min(range.1), range.0.max(range.1));
                window.paint_quad(fill(
                    rect(left, 0.0, right - left, TICKS_TOP_REMS),
                    color,
                ));
            };
            if let Some(range) =
                interval.and_then(|interval| layout.axis_range(&interval))
            {
                let (start, end) = (range.start, range.end);
                span(
                    (viewport.x_of(start, width), viewport.x_of(end, width)),
                    colors.interval,
                    window,
                );
            }
            if let Some(drag) = drag {
                span((drag.from, drag.to), colors.interval, window);
            }
            for bar in bars.iter() {
                let (top, height) = bar.lane.band_rems();
                if selected.as_deref() == Some(bar.record_id.as_str()) {
                    window.paint_quad(fill(
                        rect(
                            bar.x - 1.0,
                            top - 0.0625,
                            bar.w + 2.0,
                            height + 0.125,
                        ),
                        colors.selected,
                    ));
                }
                colors.paint(bar, rect(bar.x, top, bar.w, height), window);
            }
        }
    }

    /// Run titles, time ticks, merge counts, the unloaded-history marker,
    /// and the hover details, as text placed over the canvas.
    fn render_labels(
        &self,
        layout: &TimelineLayout,
        bars: &[Bar],
        width: f32,
        rem: Pixels,
        cx: &Context<'_, Self>,
    ) -> Vec<AnyElement> {
        if width <= 0.0 {
            return Vec::new();
        }
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let label = |x: f32, top: f32, text: SharedString| {
            div()
                .absolute()
                .left(px(x))
                .top(rems(top))
                .text_xs()
                .text_color(muted)
                .whitespace_nowrap()
                .child(text)
                .into_any_element()
        };
        // Short runs start too close together to name each one; a label
        // is skipped while it would overlap the one drawn before it.
        let room = f32::from(rem * RUN_LABEL_REMS);
        let mut next_free = f32::MIN;
        let mut labels: Vec<AnyElement> = layout
            .run_starts()
            .iter()
            .filter_map(|(index, start)| {
                let x = self.timeline.viewport.x_of(*start, width) + 3.0;
                if x < next_free {
                    return None;
                }
                next_free = x + room;
                Some(label(
                    x,
                    0.0,
                    rust_i18n::t!("Trajectory.run", index = index)
                        .into_owned()
                        .into(),
                ))
            })
            .collect();
        let visible = &self.timeline.viewport.visible;
        let domain = layout.domain();
        labels.extend((0..TICKS).map(|tick| {
            let share = tick as f64 / TICKS as f64;
            let t = visible.start + share * (visible.end - visible.start);
            let text: SharedString = match self.timeline.mode {
                DurationMode::Actual => {
                    format!("+{}", Elapsed(whole(t - domain.start)).precise())
                        .into()
                }
                DurationMode::Equal => format!("#{}", whole(t) + 1).into(),
            };
            label(
                self.timeline.viewport.x_of(t, width) + 3.0,
                TICKS_TOP_REMS,
                text,
            )
        }));
        for bar in bars {
            if bar.merged > 1 && rem * 1.5 < px(bar.w) {
                let (top, _) = bar.lane.band_rems();
                labels.push(label(
                    bar.x + 2.0,
                    top,
                    bar.merged.to_string().into(),
                ));
            }
        }
        if self.model.has_more_before() {
            labels.push(label(
                3.0,
                TICKS_TOP_REMS - 0.875,
                rust_i18n::t!("Trajectory.load_earlier").into_owned().into(),
            ));
        }
        if let Some(hover) =
            self.timeline.hover.as_ref().filter(|hover| hover.shown)
            && let Some(text) = self.hover_text(&hover.record_id, layout)
        {
            // Past the middle the details open leftwards, so they stay inside.
            let right_half = hover.at.x > width / 2.0;
            labels.push(
                div()
                    .absolute()
                    .map(|this| {
                        if right_half {
                            this.right(px(width - hover.at.x + 8.0))
                        } else {
                            this.left(px(hover.at.x + 8.0))
                        }
                    })
                    .top(px(hover.at.y + 8.0))
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .bg(theme.popover)
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.popover_foreground)
                    .whitespace_nowrap()
                    .child(text)
                    .into_any_element(),
            );
        }
        labels
    }

    /// Start, duration, and for requests the first-token latency, generation
    /// time, and throughput of one record.
    fn hover_text(&self, id: &str, layout: &TimelineLayout) -> Option<String> {
        let record = self.model.record(id)?;
        let start = record.started_at_ms?.get();
        let since = Elapsed(start.saturating_sub(whole(layout.domain().start)));
        let mut parts = vec![format!("+{}", since.precise())];
        if let Some(end) = record.ended_at_ms {
            parts.push(
                Elapsed(end.get().saturating_sub(start))
                    .precise()
                    .to_string(),
            );
        }
        if let TrajectoryRecordKind::Assistant(assistant) = &record.kind {
            let first = assistant.first_token_at_ms.get();
            let last = assistant.last_token_at_ms.get();
            let generation = last.saturating_sub(first);
            parts.push(
                rust_i18n::t!(
                    "Trajectory.first_token",
                    time =
                        Elapsed(first.saturating_sub(start)).precise().as_ref()
                )
                .into_owned(),
            );
            parts.push(format!(
                "{} {}",
                rust_i18n::t!("Record.generation"),
                Elapsed(generation).precise()
            ));
            if let Some(output) = assistant.usage.as_ref().output_tokens
                && generation > 0
            {
                parts.push(format!("{} tok/s", output * 1000 / generation));
            }
        }
        Some(parts.join(" · "))
    }

    /// The pointer in pixels from the timeline's top left.
    fn local(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self.timeline.bounds.get().origin;
        point(
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        )
    }

    /// The timeline's width in pixels.
    fn timeline_width(&self) -> f32 {
        f32::from(self.timeline.bounds.get().size.width)
    }

    /// A press starts a drag; a double click shows everything again.
    fn on_timeline_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        window.focus(&self.timeline.focus, cx);
        self.timeline.hover = None;
        if event.button == MouseButton::Left && event.click_count >= 2 {
            self.timeline.drag = None;
            self.reset_zoom(cx);
            return;
        }
        let x = self.local(event.position).x;
        self.timeline.drag = Some(TimelineDrag {
            button: event.button,
            from: x,
            to: x,
        });
    }

    /// Extends a left drag, pans with a right drag, or tracks the bar the
    /// pointer rests on.
    fn on_timeline_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let at = self.local(event.position);
        if let Some(drag) = &mut self.timeline.drag {
            let moved = at.x - drag.to;
            drag.to = at.x;
            if drag.button == MouseButton::Right {
                let width = self.timeline_width();
                if width > 0.0 {
                    self.pan_by(-f64::from(moved / width), cx);
                }
            }
            cx.notify();
            return;
        }
        let hit = self.hit_at(at).map(str::to_owned);
        match (hit, &self.timeline.hover) {
            (Some(id), Some(hover)) if hover.record_id == id => {}
            (Some(id), _) => {
                let timer = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(HOVER_DELAY).await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(hover) = &mut this.timeline.hover {
                            hover.shown = true;
                            cx.notify();
                        }
                    });
                });
                let was_shown = self
                    .timeline
                    .hover
                    .as_ref()
                    .is_some_and(|hover| hover.shown);
                self.timeline.hover = Some(
                    TimelineHover::builder()
                        .record_id(id)
                        .at(at)
                        ._timer(timer)
                        .build(),
                );
                if was_shown {
                    cx.notify();
                }
            }
            (None, Some(hover)) => {
                if hover.shown {
                    cx.notify();
                }
                self.timeline.hover = None;
            }
            (None, None) => {}
        }
    }

    /// Ends a press: a left click selects the bar under it, a left drag
    /// filters by the dragged interval, a right click clears the interval.
    fn on_timeline_up(
        &mut self,
        event: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(drag) = self.timeline.drag.take() else {
            return;
        };
        let clicked = (drag.to - drag.from).abs() < CLICK_SLOP;
        match (drag.button, clicked) {
            (MouseButton::Left, true) => {
                let at = self.local(event.position);
                if let Some(id) = self.hit_at(at).map(str::to_owned) {
                    self.select(&id, cx);
                }
            }
            (MouseButton::Left, false) => {
                if let Some(layout) = self.timeline.layout() {
                    let interval = layout.interval(
                        &self.timeline.viewport,
                        self.timeline_width(),
                        drag.from,
                        drag.to,
                    );
                    self.select_interval(Some(interval), cx);
                }
            }
            (MouseButton::Right, true) => self.select_interval(None, cx),
            _ => {}
        }
        cx.notify();
    }

    /// The wheel zooms around the pointer; with Shift it pans.
    fn on_timeline_scroll(
        &mut self,
        event: &ScrollEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let width = self.timeline_width();
        if width <= 0.0 {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        if event.modifiers.shift {
            let moved = if delta.x == px(0.) { delta.y } else { delta.x };
            self.pan_by(-f64::from(f32::from(moved) / width), cx);
        } else {
            let anchor = f64::from(self.local(event.position).x / width);
            let factor = (f64::from(f32::from(delta.y)) * ZOOM_PER_PIXEL).exp();
            self.zoom_at(factor, anchor.clamp(0.0, 1.0), cx);
        }
        cx.stop_propagation();
    }

    /// Left and right move the selection in time order; Escape clears the interval.
    fn on_timeline_key(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event.keystroke.key.as_str() {
            "left" => self.select_adjacent(-1, cx),
            "right" => self.select_adjacent(1, cx),
            "escape" => self.select_interval(None, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The record drawn under a point, by its lane.
    fn hit_at(&self, at: Point<f32>) -> Option<&str> {
        let (_, layout) = self.timeline.layout_cache.as_ref()?;
        let rem = f32::from(self.timeline.rem);
        let lane = Lane::at_rems(at.y / rem)?;
        layout.hit(&self.timeline.viewport, self.timeline_width(), at.x, lane)
    }
}

impl TrajectoryView {
    /// The layout of the loaded records and running work, rebuilt only when
    /// they or the mode changed, or while something runs and grows.
    fn timeline_layout(&mut self) -> Rc<TimelineLayout> {
        let key = LayoutKey {
            revision: self.model.revision(),
            mode: self.timeline.mode,
            in_flight: self.in_flight.clone(),
        };
        if let Some((cached, layout)) = &self.timeline.layout_cache
            && *cached == key
            && !layout.running()
        {
            return Rc::clone(layout);
        }
        let layout = Rc::new(TimelineLayout::new(
            self.model.records(),
            &self.in_flight,
            crate::now_ms(),
            self.timeline.mode,
        ));
        self.timeline.layout_cache = Some((key, Rc::clone(&layout)));
        layout
    }

    /// Switches the overview between real time and one unit per record.
    pub fn set_duration_mode(
        &mut self,
        mode: DurationMode,
        cx: &mut Context<'_, Self>,
    ) {
        if self.timeline.mode != mode {
            self.timeline.mode = mode;
            self.timeline.zoomed = false;
            cx.notify();
        }
    }

    /// Zooms by `factor` around `anchor`, a 0–1 position across the overview.
    pub fn zoom_at(
        &mut self,
        factor: f64,
        anchor: f64,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(layout) = self.timeline.layout() else {
            return;
        };
        self.timeline.viewport.zoom(factor, anchor, layout.domain());
        self.timeline.zoomed =
            self.timeline.viewport.visible != *layout.domain();
        cx.notify();
    }

    /// Moves the zoomed overview by `fraction` of its width.
    pub fn pan_by(&mut self, fraction: f64, cx: &mut Context<'_, Self>) {
        let Some(layout) = self.timeline.layout() else {
            return;
        };
        self.timeline.viewport.pan(fraction, layout.domain());
        cx.notify();
    }

    /// Shows every loaded record again.
    pub fn reset_zoom(&mut self, cx: &mut Context<'_, Self>) {
        self.timeline.zoomed = false;
        cx.notify();
    }

    /// Keeps only records meeting `interval` in the ledger, or all of them.
    pub fn select_interval(
        &mut self,
        interval: Option<TimeInterval>,
        cx: &mut Context<'_, Self>,
    ) {
        self.model.set_interval(interval);
        self.sync_rows(cx);
    }
}

/// Theme colors of the timeline.
#[derive(TypedBuilder)]
struct BarColors {
    request: Hsla,
    tool: Hsla,
    event: Hsla,
    danger: Hsla,
    separator: Hsla,
    interval: Hsla,
    selected: Hsla,
}

impl BarColors {
    /// Paints one bar: a request's wait lighter than its generation, a
    /// failure in the danger color, a running bar with a dashed end.
    fn paint(&self, bar: &Bar, area: Bounds<Pixels>, window: &mut Window) {
        let base = match (bar.lane, bar.failed) {
            (_, true) => self.danger,
            (Lane::Request, false) => self.request,
            (Lane::Tool(_), false) => self.tool,
            (Lane::Event, false) => self.event,
        };
        let (left, w) = (area.origin.x, f32::from(area.size.width));
        if let Some(first) = bar.first_token_x.filter(|_| !bar.failed) {
            let wait = (first - bar.x).clamp(0.0, w);
            window.paint_quad(fill(
                Bounds::new(area.origin, size(px(wait), area.size.height)),
                base.opacity(0.35),
            ));
            window.paint_quad(fill(
                Bounds::new(
                    point(left + px(wait), area.origin.y),
                    size(px(w - wait), area.size.height),
                ),
                base,
            ));
        } else if bar.running && w > 12.0 {
            // The solid part, then dashes for the time still running.
            window.paint_quad(fill(
                Bounds::new(area.origin, size(px(w - 12.0), area.size.height)),
                base,
            ));
            for dash in 0..3 {
                let x = w - 12.0 + 4.0 * dash as f32;
                window.paint_quad(fill(
                    Bounds::new(
                        point(left + px(x), area.origin.y),
                        size(px(2.0), area.size.height),
                    ),
                    base,
                ));
            }
        } else {
            window.paint_quad(fill(area, base));
        }
    }
}
