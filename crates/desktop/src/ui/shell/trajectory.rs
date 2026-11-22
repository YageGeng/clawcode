//! The shell's side of the trajectory view (M4a spec §4, §6, §7): the chat /
//! trajectory switch and its shortcut, the shared overview setting, and
//! opening a record in the panel.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::{AnyElement, Context, Entity, IntoElement, Window};

use crate::actions::ToggleTrajectory;
use crate::state::PanelTabKey;
use crate::ui::session::{SessionView, ViewMode};
use crate::ui::trajectory::TimelineVisible;
use crate::ui::{AppView, TrajectoryEvent};

impl AppView {
    /// The chat / trajectory switch of `view`: the active session tab's in
    /// the main card, or a subagent's in the panel. `id` names the switch.
    pub(in crate::ui) fn render_view_switch(
        &self,
        id: &'static str,
        view: Entity<SessionView>,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let mode = view.read(cx).view_mode();
        let shell = cx.entity().downgrade();
        TabBar::new(id)
            .segmented()
            .small()
            .selected_index(usize::from(mode == ViewMode::Trajectory))
            .child(
                Tab::new()
                    .label(rust_i18n::t!("Trajectory.view_chat").into_owned()),
            )
            .child(Tab::new().label(
                rust_i18n::t!("Trajectory.view_trajectory").into_owned(),
            ))
            .on_click(move |index, window, cx| {
                let mode = if *index == 1 {
                    ViewMode::Trajectory
                } else {
                    ViewMode::Chat
                };
                view.update(cx, |view, cx| {
                    view.set_view_mode(mode, window, cx)
                });
                // The switch is drawn by the shell, which redraws to show it.
                let _ = shell.update(cx, |_, cx| cx.notify());
            })
            .into_any_element()
    }

    /// Shows the chat or the trajectory in the active session tab.
    pub(in crate::ui) fn set_active_view_mode(
        &mut self,
        mode: ViewMode,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if let Some(view) = self.active_view().cloned() {
            view.update(cx, |view, cx| view.set_view_mode(mode, window, cx));
            cx.notify();
        }
    }

    /// `ToggleTrajectory`: switches the active session tab between its chat
    /// and its trajectory.
    pub(in crate::ui) fn on_toggle_trajectory(
        &mut self,
        _: &ToggleTrajectory,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let Some(view) = self.active_view() else {
            return;
        };
        let mode = match view.read(cx).view_mode() {
            ViewMode::Chat => ViewMode::Trajectory,
            ViewMode::Trajectory => ViewMode::Chat,
        };
        self.set_active_view_mode(mode, window, cx);
    }

    /// Opens the record detail tab for an opened record, or saves the
    /// overview setting every trajectory view reads.
    pub(in crate::ui) fn on_trajectory_event(
        &mut self,
        event: &TrajectoryEvent,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        match event {
            TrajectoryEvent::Open(_) => {
                self.open_panel_tab(PanelTabKey::Record, window, cx);
            }
            TrajectoryEvent::TimelineToggled(open) => {
                self.state.layout.timeline_open = *open;
                cx.set_global(TimelineVisible(*open));
                self.schedule_save(cx);
            }
        }
    }
}
