//! Shared picker state, keyboard behavior, and picker implementations.

mod agent;
mod model;

use crossterm::event::KeyCode;

pub(crate) use agent::{
    AgentPicker, handle_agent_picker_key, render_agent_picker,
};
pub(crate) use model::{
    ModelPicker, handle_model_picker_key, render_model_picker, switch_model,
};

/// Result of a generic picker key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickerAction {
    None,
    Submit(usize),
}

/// Shared focus and selected-row state for inline pickers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PickerState {
    visible: bool,
    focused: bool,
    selected_index: usize,
}

/// Common behavior for inline pickers that manage their own selection state.
pub(crate) trait Picker {
    /// Returns immutable shared picker state.
    fn state(&self) -> &PickerState;

    /// Returns mutable shared picker state.
    fn state_mut(&mut self) -> &mut PickerState;

    /// Opens the picker while preserving its current selected index.
    fn open(&mut self, entry_count: usize) {
        let state = self.state_mut();
        state.visible = true;
        state.focused = true;
        state.clamp_selection(entry_count);
    }

    /// Closes the picker and returns focus to the composer.
    fn close(&mut self) {
        let state = self.state_mut();
        state.visible = false;
        state.focused = false;
    }

    /// Moves selection to the previous entry with wraparound.
    fn move_previous(&mut self, entry_count: usize) {
        self.state_mut().move_previous(entry_count);
    }

    /// Moves selection to the next entry with wraparound.
    fn move_next(&mut self, entry_count: usize) {
        self.state_mut().move_next(entry_count);
    }

    /// Handles shared picker navigation keys.
    fn handle_key(
        &mut self,
        code: KeyCode,
        entry_count: usize,
    ) -> PickerAction {
        match code {
            KeyCode::Up => {
                self.move_previous(entry_count);
                PickerAction::None
            }
            KeyCode::Down => {
                self.move_next(entry_count);
                PickerAction::None
            }
            KeyCode::Enter if entry_count > 0 => {
                PickerAction::Submit(self.state().selected_index())
            }
            KeyCode::Enter => PickerAction::None,
            KeyCode::Esc => {
                self.close();
                PickerAction::None
            }
            _ => PickerAction::None,
        }
    }

    /// Returns whether the picker is visible.
    fn is_visible(&self) -> bool {
        self.state().is_visible()
    }

    /// Returns whether the picker is focused.
    fn is_focused(&self) -> bool {
        self.state().is_focused()
    }

    /// Returns the selected entry index.
    fn selected_index(&self) -> usize {
        self.state().selected_index()
    }
}

impl PickerState {
    /// Moves selection to the previous entry with wraparound.
    fn move_previous(&mut self, entry_count: usize) {
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
    fn move_next(&mut self, entry_count: usize) {
        if entry_count == 0 {
            self.selected_index = 0;
            return;
        }
        self.selected_index = (self.selected_index + 1) % entry_count;
    }

    /// Returns whether the picker is visible.
    fn is_visible(&self) -> bool {
        self.visible
    }

    /// Returns whether the picker is focused.
    fn is_focused(&self) -> bool {
        self.focused
    }

    /// Returns the selected entry index.
    fn selected_index(&self) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[derive(Default)]
    struct TestPicker {
        state: PickerState,
    }

    impl Picker for TestPicker {
        /// Returns shared picker selection state for tests.
        fn state(&self) -> &PickerState {
            &self.state
        }

        /// Returns mutable shared picker selection state for tests.
        fn state_mut(&mut self) -> &mut PickerState {
            &mut self.state
        }
    }

    /// Verifies the picker trait provides shared focus and key behavior.
    #[test]
    fn picker_trait_handles_shared_navigation_keys() {
        let mut picker = TestPicker::default();

        picker.open(2);
        assert!(picker.is_focused());
        assert_eq!(picker.selected_index(), 0);

        assert_eq!(picker.handle_key(KeyCode::Up, 2), PickerAction::None);
        assert_eq!(picker.selected_index(), 1);

        assert_eq!(
            picker.handle_key(KeyCode::Enter, 2),
            PickerAction::Submit(1)
        );

        assert_eq!(picker.handle_key(KeyCode::Esc, 2), PickerAction::None);
        assert!(!picker.is_focused());
    }

    /// Verifies Enter does not submit when no picker entries exist.
    #[test]
    fn picker_trait_ignores_enter_without_entries() {
        let mut picker = TestPicker::default();
        picker.open(0);

        assert_eq!(picker.handle_key(KeyCode::Enter, 0), PickerAction::None);
    }
}
