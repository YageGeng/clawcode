//! Inline agent picker state and rendering helpers.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, layout::Rect};

use crate::ui::agent_navigation::AgentPickerStatus;
use crate::ui::session_router::SessionRouterState;
use crate::ui::theme::Theme;

/// Focus and selection state for the inline `/agent` picker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AgentPickerPanelState {
    visible: bool,
    focused: bool,
    selected_index: usize,
}

impl AgentPickerPanelState {
    /// Opens the picker while preserving its current selected index.
    pub(crate) fn open(&mut self, entry_count: usize) {
        self.visible = true;
        self.focused = true;
        self.clamp_selection(entry_count);
    }

    /// Closes the picker and returns focus to the composer.
    pub(crate) fn close(&mut self) {
        self.visible = false;
        self.focused = false;
    }

    /// Moves selection to the previous entry with wraparound.
    pub(crate) fn move_previous(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = if self.selected_index == 0 {
            entry_count - 1
        } else {
            self.selected_index - 1
        };
    }

    /// Moves selection to the next entry with wraparound.
    pub(crate) fn move_next(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = (self.selected_index + 1) % entry_count;
    }

    /// Returns whether the picker is visible.
    pub(crate) fn is_visible(&self) -> bool {
        self.visible
    }

    /// Returns whether the picker is focused.
    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    /// Returns the selected entry index.
    pub(crate) fn selected_index(&self) -> usize {
        self.selected_index
    }

    /// Clamp selection to the available entry range.
    fn clamp_selection(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= entry_count {
            self.selected_index = entry_count - 1;
        }
    }
}

/// Renders the inline agent picker below the composer.
pub(crate) fn render_agent_picker(
    frame: &mut Frame<'_>,
    area: Rect,
    router: &SessionRouterState,
    theme: &Theme,
) {
    if area.height == 0 {
        return;
    }

    let entries = router.agent_navigation().ordered_entries();
    let selected_index = router.agent_picker_selected_index();
    let lines = entries
        .into_iter()
        .enumerate()
        .take(usize::from(area.height))
        .map(|(index, entry)| {
            let is_selected = index == selected_index;
            let is_active = entry.session_id() == router.active_session_id();
            let marker = if is_selected { ">" } else { " " };
            let current = if is_active { " current" } else { "" };
            let status = status_symbol(entry.status());
            let style = if is_selected {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(vec![
                Span::styled(format!("{marker} {status} "), style),
                Span::styled(entry.label(), style),
                Span::styled(current.to_string(), Style::default().fg(theme.muted)),
            ])
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(theme.surface)),
        area,
    );
}

/// Return a compact status token for one picker row.
fn status_symbol(status: AgentPickerStatus) -> &'static str {
    match status {
        AgentPickerStatus::Pending => "!",
        AgentPickerStatus::Running => "*",
        AgentPickerStatus::Completed => "v",
        AgentPickerStatus::Errored => "x",
        AgentPickerStatus::Closed => "-",
        AgentPickerStatus::Unknown => "?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies picker focus opens and arrow movement wraps through entries.
    #[test]
    fn picker_focus_moves_selection_with_wraparound() {
        let mut picker = AgentPickerPanelState::default();
        picker.open(2);

        assert!(picker.is_focused());
        assert_eq!(picker.selected_index(), 0);

        picker.move_previous(2);
        assert_eq!(picker.selected_index(), 1);

        picker.move_next(2);
        assert_eq!(picker.selected_index(), 0);
    }
}
