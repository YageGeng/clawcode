//! Where the panel shows: beside the main card, as a sheet, or not at all.

use gpui_kit::{Context, Window};

use crate::state::LayoutState;
use crate::ui::AppView;

/// Narrowest main card beside the panel before the panel becomes a sheet, in px at 16px rem.
const MIN_MAIN_WIDTH: f32 = 480.0;
/// Horizontal `sm` gaps around the three cards, in px at 16px rem.
const CARD_GAPS: f32 = 32.0;

/// How the right panel shows in the current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelPresentation {
    /// Beside the main card.
    Inline,
    /// Over the main card, from the right edge.
    Sheet,
    /// Not shown.
    Hidden,
}

impl AppView {
    /// Whether the active panel shows, and how, for this window's width.
    pub fn panel_presentation(&self, window: &Window) -> PanelPresentation {
        if !self.panel().is_some_and(|panel| panel.open) {
            return PanelPresentation::Hidden;
        }
        let scale = f32::from(window.rem_size()) / 16.0;
        let main = f32::from(window.viewport_size().width)
            - self.session_card_width()
            - self.panel_width(window)
            - CARD_GAPS * scale;
        if main < MIN_MAIN_WIDTH * scale {
            PanelPresentation::Sheet
        } else {
            PanelPresentation::Inline
        }
    }

    /// The session card's width while it shows.
    pub(super) fn session_card_width(&self) -> f32 {
        let layout = &self.state.layout;
        if layout.sidebar_open {
            layout.sidebar_width()
        } else {
            0.0
        }
    }

    /// The panel's width, capped so the main card keeps its minimum beside it.
    pub(in crate::ui) fn panel_width(&self, window: &Window) -> f32 {
        let scale = f32::from(window.rem_size()) / 16.0;
        let room = f32::from(window.viewport_size().width)
            - self.session_card_width()
            - (MIN_MAIN_WIDTH + CARD_GAPS) * scale;
        self.state
            .layout
            .panel_width()
            .min(room.max(LayoutState::MIN_PANEL_WIDTH))
    }

    /// Moves focus into the sheet when the panel becomes one (also by
    /// narrowing the window), so Esc closes it instead of reaching the covered
    /// composer; gives focus back to the shell when a focused sheet goes away.
    pub(in crate::ui) fn sync_panel_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        let sheet = self.panel_presentation(window) == PanelPresentation::Sheet;
        if sheet == self.panels.was_sheet {
            return;
        }
        self.panels.was_sheet = sheet;
        let target = if sheet {
            self.panels.sheet_focus.clone()
        } else if self.panels.sheet_focus.contains_focused(window, cx) {
            self.focus_handle.clone()
        } else {
            return;
        };
        cx.defer_in(window, move |_, window, cx| window.focus(&target, cx));
    }

    /// Focuses the sheet when the panel opens as one, so Esc reaches it.
    pub(super) fn focus_panel_sheet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.panel_presentation(window) == PanelPresentation::Sheet {
            window.focus(&self.panels.sheet_focus.clone(), cx);
        }
    }
}
