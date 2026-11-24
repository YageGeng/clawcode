//! The trajectory view of one session (M4a spec §5): the toolbar, the time
//! overview, and the ledger table. Loading lives in `loading`, the overview
//! in `timeline`.

mod loading;
mod table;

pub(in crate::ui) use table::KindLook;
mod timeline;
mod toolbar;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::label::Label;
use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::table::{DataTable, TableEvent, TableState};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    Pixels, Render, Styled as _, Subscription, TestSupportExt as _, Window,
    div, prelude::FluentBuilder as _, px,
};
use protocol::{AssistantRecord, ContentHash, RequestContent, RequestRecord};
use typed_builder::TypedBuilder;

use self::loading::Loader;
use self::table::{LedgerTable, LineBuilder};
use self::timeline::TimelineState;
use crate::backend::AcpClient;
use crate::projection::ProjectionChange;
use crate::session::SessionHandle;
use crate::trajectory::{InFlight, LedgerRow, TrajectoryModel};

/// Whether trajectory views show their time overview: one setting for
/// every session, kept by the shell (M4a spec §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimelineVisible(pub bool);

impl Global for TimelineVisible {}

impl TimelineVisible {
    /// The current setting; shown until the shell sets it.
    pub fn get(cx: &gpui_kit::App) -> bool {
        cx.try_global::<Self>().is_none_or(|visible| visible.0)
    }
}

/// Whether the trajectory could be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LoadState {
    /// Nothing is being read.
    #[default]
    Idle,
    /// The first page is being read.
    Loading,
    /// The first page could not be read.
    Failed(String),
    /// The history database is not ready.
    Unavailable,
}

/// A request content as the record detail reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentState {
    /// Being read.
    Loading,
    /// Read.
    Ready(Arc<RequestContent>),
    /// The session's log does not hold it.
    Missing,
    /// The read failed.
    Failed(String),
}

/// What the trajectory view asks its owners to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrajectoryEvent {
    /// Show the record with this id in the record detail tab.
    Open(String),
    /// The user showed or hid the time overview.
    TimelineToggled(bool),
}

/// The title of one model request: `Request #12 · deepseek-flash`.
pub(in crate::ui) trait RequestTitle {
    /// The localized title.
    fn title(&self) -> String;
}

impl RequestTitle for RequestRecord {
    /// Number and model of the request.
    fn title(&self) -> String {
        request_title(self.index, &self.snapshot.options.model_id)
    }
}

impl RequestTitle for AssistantRecord {
    /// Number and model of the request that produced it.
    fn title(&self) -> String {
        request_title(self.request_index, &self.model_id)
    }
}

/// `Request #index · model`.
fn request_title(index: u32, model: &str) -> String {
    rust_i18n::t!("Trajectory.request", index = index, model = model)
        .into_owned()
}

/// One session's trajectory: loaded records, the ledger, and its loading.
#[derive(TypedBuilder)]
pub struct TrajectoryView {
    session: Entity<SessionHandle>,
    #[builder(default)]
    model: TrajectoryModel,
    table: Entity<TableState<LedgerTable>>,
    /// Reads pages and keeps their state.
    loader: Loader,
    /// The time overview's state.
    timeline: TimelineState,
    /// Request contents read so far, by hash; shared by every record.
    #[builder(default)]
    contents: HashMap<ContentHash, ContentState>,
    /// The running work the last rows showed.
    #[builder(default)]
    in_flight: Vec<InFlight>,
    /// The toolbar's width on the last frame; wide until first measured.
    #[builder(default = Rc::new(Cell::new(px(f32::MAX))))]
    toolbar_width: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TrajectoryEvent> for TrajectoryView {}

impl TrajectoryView {
    /// Creates the view of `session` and reads its newest page.
    pub fn new(
        session: Entity<SessionHandle>,
        client: Option<AcpClient>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let ledger =
            LedgerTable::new(cx.entity().downgrade(), window.rem_size());
        let table = cx.new(|cx| {
            TableState::new(ledger, window, cx)
                .col_movable(false)
                .sortable(false)
                .loop_selection(false)
        });
        let subscriptions = vec![
            cx.subscribe(&table, Self::on_table_event),
            cx.observe_global::<TimelineVisible>(|_, cx| cx.notify()),
        ];
        let mut view = Self::builder()
            .session(session)
            .table(table)
            .loader(Loader::new(client))
            .timeline(TimelineState::new(cx.focus_handle()))
            ._subscriptions(subscriptions)
            .build();
        view.fetch(protocol::TrajectoryCursor::Tail, cx);
        view
    }

    /// The loaded records and the ledger's view state.
    pub fn model(&self) -> &TrajectoryModel {
        &self.model
    }

    /// The session this trajectory belongs to.
    pub fn session_id(&self, cx: &gpui_kit::App) -> crate::backend::SessionId {
        self.session.read(cx).id().clone()
    }

    /// Selects the record `id` in the ledger.
    pub fn select(&mut self, id: &str, cx: &mut Context<'_, Self>) {
        self.model.select(id);
        self.sync_selection(cx);
    }

    /// Moves the selection `delta` record rows among the rows shown.
    pub fn select_adjacent(
        &mut self,
        delta: isize,
        cx: &mut Context<'_, Self>,
    ) {
        let table = self.table.read(cx);
        let rows = table.delegate().lines.iter().map(|line| &line.row);
        self.model.select_adjacent(rows, delta);
        self.sync_selection(cx);
    }

    /// Follows the live session: refreshes when the history gained records,
    /// and redraws the rows when the running work changed.
    pub fn session_changed(
        &mut self,
        change: &ProjectionChange,
        cx: &mut Context<'_, Self>,
    ) {
        if change.history {
            self.refresh(cx);
        }
        if (change.run_state || change.structural)
            && self.current_in_flight(cx) != self.in_flight
        {
            self.sync_following(cx);
        }
    }

    /// The running work of the session now.
    fn current_in_flight(&self, cx: &gpui_kit::App) -> Vec<InFlight> {
        InFlight::from_projection(
            self.session.read(cx).projection(),
            self.model.last_ended_ms(),
        )
    }

    /// Rebuilds the rows for new data, staying at the ledger's end when it
    /// was there.
    fn sync_following(&mut self, cx: &mut Context<'_, Self>) {
        let follow = self.at_bottom(cx);
        self.sync_rows(cx);
        if follow {
            self.scroll_to_latest(cx);
        }
    }

    /// Rebuilds the ledger lines from the model and the running work; the
    /// view stays where it is.
    fn sync_rows(&mut self, cx: &mut Context<'_, Self>) {
        self.in_flight = self.current_in_flight(cx);
        let rows = self.model.rows(&self.in_flight);
        let builder = LineBuilder::new(&self.model, &self.in_flight);
        let lines = rows.iter().map(|row| builder.line(row)).collect();
        self.table.update(cx, |table, cx| {
            table.delegate_mut().lines = lines;
            table.refresh(cx);
        });
        self.sync_selection(cx);
    }

    /// Points the table's selected row at the model's selected record; the
    /// lines stay as they are.
    fn sync_selection(&mut self, cx: &mut Context<'_, Self>) {
        let selected = self.model.selected_record().map(|record| &record.id);
        self.table.update(cx, |table, cx| {
            let row = selected.and_then(|selected| {
                table.delegate().lines.iter().position(|line| {
                    matches!(&line.row, LedgerRow::Record { id } if id == selected)
                })
            });
            // Re-selecting scrolls, so only a moved selection is set again.
            match row {
                Some(ix) if table.selected_row() != Some(ix) => {
                    table.set_selected_row(ix, cx)
                }
                None if table.selected_row().is_some() => {
                    table.clear_selection(cx)
                }
                _ => cx.notify(),
            }
        });
        cx.notify();
    }

    /// Acts on a click on a row that is not a record.
    fn activate(&mut self, row: &LedgerRow, cx: &mut Context<'_, Self>) {
        match row {
            LedgerRow::LoadEarlier => self.load_earlier(cx),
            LedgerRow::RunHeader { key, .. } => {
                self.model.toggle_run(key.clone());
                self.sync_rows(cx);
            }
            LedgerRow::CallGroup { request_index, .. } => {
                self.model.toggle_call_group(*request_index);
                self.sync_rows(cx);
            }
            LedgerRow::Record { .. } | LedgerRow::InFlight(_) => {}
        }
    }

    /// The ledger row at table row `ix`.
    fn row_at(&self, ix: usize, cx: &gpui_kit::App) -> Option<LedgerRow> {
        let table = self.table.read(cx);
        Some(table.delegate().lines.get(ix)?.row.clone())
    }

    /// Keeps the model's selection in step with the table's, and opens a
    /// record on double click.
    fn on_table_event(
        &mut self,
        _: Entity<TableState<LedgerTable>>,
        event: &TableEvent,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            TableEvent::SelectRow(ix) => {
                if let Some(LedgerRow::Record { id }) = self.row_at(*ix, cx) {
                    self.model.select(&id);
                    cx.notify();
                }
            }
            TableEvent::DoubleClickedRow(ix) => {
                if let Some(LedgerRow::Record { id }) = self.row_at(*ix, cx) {
                    cx.emit(TrajectoryEvent::Open(id));
                }
            }
            _ => {}
        }
    }

    /// Enter opens the selected record, or loads earlier records on that row.
    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if event.keystroke.key != "enter" {
            return;
        }
        let selected = self.table.read(cx).selected_row();
        match selected.and_then(|ix| self.row_at(ix, cx)) {
            Some(LedgerRow::Record { id }) => {
                cx.emit(TrajectoryEvent::Open(id));
                cx.stop_propagation();
            }
            Some(LedgerRow::LoadEarlier) => {
                self.load_earlier(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    /// Rebuilds the rows after earlier records arrived, keeping the rows in
    /// view where they were: the first row in view is found again and the
    /// ledger moves by the rows that came before it. Re-selecting a moved
    /// selection would scroll to it, so that scroll is dropped.
    fn sync_rows_keeping_view(&mut self, cx: &mut Context<'_, Self>) {
        let anchor = {
            let table = self.table.read(cx);
            let lines = &table.delegate().lines;
            let scroll = table.vertical_scroll_handle.0.borrow();
            let handle = &scroll.base_handle;
            // Every row has the same height; the content's height tells it,
            // once the content overflows. A ledger that fits has nothing to keep.
            let content = handle.max_offset().y + handle.bounds().size.height;
            let height = content / lines.len().max(1) as f32;
            let first = usize::try_from(crate::utils::whole(f64::from(
                -handle.offset().y / height,
            )))
            .unwrap_or(0);
            // The first record in view: "load earlier" and Run titles stay
            // at the top as rows come in above them, so they cannot anchor.
            (handle.max_offset().y > px(0.))
                .then(|| {
                    lines.iter().enumerate().skip(first).find(|(_, line)| {
                        matches!(line.row, LedgerRow::Record { .. })
                    })
                })
                .flatten()
                .map(|(index, line)| (line.row.clone(), index, height))
        };
        self.sync_rows(cx);
        let Some((row, before, height)) = anchor else {
            return;
        };
        self.table.update(cx, |table, cx| {
            let Some(after) = table
                .delegate()
                .lines
                .iter()
                .position(|line| line.row == row)
            else {
                return;
            };
            let mut scroll = table.vertical_scroll_handle.0.borrow_mut();
            scroll.deferred_scroll_to_item = None;
            let mut offset = scroll.base_handle.offset();
            offset.y -= height * (after as f32 - before as f32);
            scroll.base_handle.set_offset(offset);
            cx.notify();
        });
    }

    /// Whether the ledger shows its last row.
    fn at_bottom(&self, cx: &gpui_kit::App) -> bool {
        let scroll = self.table.read(cx).vertical_scroll_handle.0.borrow();
        let handle = &scroll.base_handle;
        // The offset grows negative as the list scrolls down.
        -handle.offset().y + px(1.) >= handle.max_offset().y
    }

    /// Scrolls the ledger to its last row.
    fn scroll_to_latest(&mut self, cx: &mut Context<'_, Self>) {
        self.table.update(cx, |table, cx| {
            if let Some(last) = table.delegate().lines.len().checked_sub(1) {
                table.scroll_to_row(last, cx);
            }
        });
    }

    /// The table area: skeleton, state message, or the ledger.
    fn render_body(&self, cx: &mut Context<'_, Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let (muted, danger) = (theme.muted_foreground, theme.danger);
        let notice = |text: String, retry: bool, cx: &mut Context<'_, Self>| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .child(Label::new(text).text_color(muted))
                .when(retry, |this| {
                    this.child(
                        Button::new("trajectory-retry")
                            .small()
                            .label(
                                rust_i18n::t!("Trajectory.retry").to_string(),
                            )
                            .on_click(
                                cx.listener(|this, _, _, cx| this.retry(cx)),
                            ),
                    )
                })
                .into_any_element()
        };
        let empty = self.model.records().is_empty();
        match &self.loader.load {
            LoadState::Loading if empty => {
                return div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .children((0..8).map(|_| Skeleton::new().h_6().w_full()))
                    .into_any_element();
            }
            LoadState::Unavailable => {
                return notice(
                    rust_i18n::t!("Trajectory.unavailable").into_owned(),
                    true,
                    cx,
                );
            }
            LoadState::Failed(error) if empty => {
                return notice(
                    rust_i18n::t!("Trajectory.failed_load", error = error)
                        .into_owned(),
                    true,
                    cx,
                );
            }
            _ => {}
        }
        let lines = self.table.read(cx).delegate().lines.len();
        if lines == 0 && !self.model.is_filtering() {
            return notice(
                rust_i18n::t!("Trajectory.empty").into_owned(),
                false,
                cx,
            );
        }
        let following = self.at_bottom(cx);
        div()
            .id("trajectory-table")
            .test_support()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .relative()
            .on_key_down(cx.listener(Self::on_key_down))
            // The "back to latest" button follows the ledger's scroll position.
            .on_scroll(
                cx.listener(|_, _: &gpui_kit::ScrollEvent, _, cx| cx.notify()),
            )
            .child(div().flex_1().min_h_0().child(
                DataTable::new(&self.table).accessibility_label(
                    rust_i18n::t!("Trajectory.view_trajectory").to_string(),
                ),
            ))
            .when_some(self.loader.refresh_error.clone(), |this, error| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(cx.theme().border)
                        .child(
                            Label::new(
                                rust_i18n::t!(
                                    "Trajectory.refresh_failed",
                                    error = error
                                )
                                .into_owned(),
                            )
                            .text_sm()
                            .text_color(danger),
                        )
                        .child(
                            Button::new("trajectory-retry")
                                .xsmall()
                                .label(
                                    rust_i18n::t!("Trajectory.retry")
                                        .to_string(),
                                )
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.retry(cx)
                                    }),
                                ),
                        ),
                )
            })
            .when(!following && lines > 0, |this| {
                // Blocks the mouse over the ledger: otherwise the row beneath takes the hover, and its click
                // selects that row, which overrides the scroll to the end.
                this.child(
                    div().absolute().bottom_3().right_4().occlude().child(
                        Button::new("trajectory-latest")
                            .small()
                            .label(
                                rust_i18n::t!("Trajectory.latest").to_string(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.scroll_to_latest(cx);
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }
}

impl Render for TrajectoryView {
    /// The toolbar above the ledger; only the ledger scrolls.
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        let timeline = self.render_timeline(window, cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .min_h_0()
            .child(self.render_toolbar(window, cx))
            .children(timeline)
            .child(self.render_body(cx))
    }
}
