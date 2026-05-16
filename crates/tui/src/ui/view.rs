//! View-only state for local TUI scrolling.

/// Mutable UI-only state for transcript scrolling.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct ViewState {
    /// Manual transcript distance from the latest rendered transcript content.
    #[builder(default)]
    transcript_scroll: u16,
    /// Whether the transcript should follow the newest rendered content.
    #[builder(default = true)]
    follow_tail: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl ViewState {
    /// Returns the manual transcript distance from the latest rendered content.
    pub fn transcript_scroll(&self) -> u16 {
        self.transcript_scroll
    }

    /// Returns whether transcript rendering follows the latest content.
    pub fn is_following_tail(&self) -> bool {
        self.follow_tail
    }

    /// Scrolls transcript history up by one page and disables tail following.
    pub fn scroll_page_up(&mut self, page_height: u16) {
        self.transcript_scroll = self.transcript_scroll.saturating_add(page_height.max(1));
        self.follow_tail = false;
    }

    /// Scrolls transcript history down by one page while staying in manual mode.
    pub fn scroll_page_down(&mut self, page_height: u16) {
        self.transcript_scroll = self.transcript_scroll.saturating_sub(page_height.max(1));
        self.follow_tail = false;
    }

    /// Scrolls to the oldest rendered transcript content.
    pub fn scroll_top(&mut self) {
        self.transcript_scroll = u16::MAX;
        self.follow_tail = false;
    }

    /// Re-enables automatic tail following for transcript rendering.
    pub fn follow_bottom(&mut self) {
        self.transcript_scroll = 0;
        self.follow_tail = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the initial view follows the transcript tail.
    #[test]
    fn view_state_defaults_to_tail_follow() {
        let view = ViewState::default();

        assert_eq!(view.transcript_scroll(), 0);
        assert!(view.is_following_tail());
    }

    /// Verifies manual scrolling disables automatic tail following.
    #[test]
    fn view_state_page_up_disables_tail_follow() {
        let mut view = ViewState::default();

        view.scroll_page_up(12);

        assert_eq!(view.transcript_scroll(), 12);
        assert!(!view.is_following_tail());
    }

    /// Verifies jumping to the bottom re-enables automatic tail following.
    #[test]
    fn view_state_follow_bottom_resets_scroll_mode() {
        let mut view = ViewState::default();
        view.scroll_page_up(12);

        view.follow_bottom();

        assert_eq!(view.transcript_scroll(), 0);
        assert!(view.is_following_tail());
    }

    /// Verifies top scrolling stores a sentinel distance that render code can clamp.
    #[test]
    fn view_state_scroll_top_uses_clamped_sentinel() {
        let mut view = ViewState::default();

        view.scroll_top();

        assert_eq!(view.transcript_scroll(), u16::MAX);
        assert!(!view.is_following_tail());
    }
}
