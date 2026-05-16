//! Editable prompt composer state.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Describes the UI action requested after handling a composer key event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerAction {
    /// The TUI should redraw after the composer handled the key event.
    Redraw,
    /// The current prompt text should be submitted to the runtime.
    Submit(String),
    /// The key event was not handled by the composer.
    Ignored,
}

/// Stores editable prompt text and the current cursor byte offset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composer {
    /// The editable prompt buffer.
    text: String,
    /// The cursor position as a UTF-8 byte offset into `text`.
    cursor: usize,
}

impl Composer {
    /// Returns the current prompt text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the current cursor byte offset.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns whether the prompt buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Clears the prompt buffer and resets the cursor.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Inserts text at the current cursor position.
    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    /// Handles a keyboard event and returns the requested composer action.
    pub fn handle_key(&mut self, key: KeyEvent) -> ComposerAction {
        // Raw terminal streams can include key-up events; only key-down and
        // repeat events should drive editable composer state.
        if key.kind == KeyEventKind::Release {
            return ComposerAction::Ignored;
        }

        match (key.code, key.modifiers) {
            (KeyCode::Enter, KeyModifiers::NONE) => {
                let submitted = self.text.trim_end().to_string();
                self.clear();
                ComposerAction::Submit(submitted)
            }
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.insert_str("\n");
                ComposerAction::Redraw
            }
            (KeyCode::Char(ch), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                self.insert_str(&ch.to_string());
                ComposerAction::Redraw
            }
            (KeyCode::Backspace, KeyModifiers::NONE) => {
                self.delete_before_cursor();
                ComposerAction::Redraw
            }
            (KeyCode::Delete, KeyModifiers::NONE) => {
                self.delete_at_cursor();
                ComposerAction::Redraw
            }
            (KeyCode::Left, KeyModifiers::NONE) => {
                self.move_left();
                ComposerAction::Redraw
            }
            (KeyCode::Right, KeyModifiers::NONE) => {
                self.move_right();
                ComposerAction::Redraw
            }
            (KeyCode::Home, KeyModifiers::NONE) => {
                self.cursor = 0;
                ComposerAction::Redraw
            }
            (KeyCode::End, KeyModifiers::NONE) => {
                self.cursor = self.text.len();
                ComposerAction::Redraw
            }
            _ => ComposerAction::Ignored,
        }
    }

    /// Deletes the UTF-8 character immediately before the cursor.
    fn delete_before_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let previous = self.previous_boundary();
        self.text.drain(previous..self.cursor);
        self.cursor = previous;
    }

    /// Deletes the UTF-8 character at the cursor.
    fn delete_at_cursor(&mut self) {
        if self.cursor == self.text.len() {
            return;
        }

        let next = self.next_boundary();
        self.text.drain(self.cursor..next);
    }

    /// Moves the cursor one UTF-8 character to the left.
    fn move_left(&mut self) {
        self.cursor = self.previous_boundary();
    }

    /// Moves the cursor one UTF-8 character to the right.
    fn move_right(&mut self) {
        self.cursor = self.next_boundary();
    }

    /// Finds the previous valid UTF-8 character boundary before the cursor.
    fn previous_boundary(&self) -> usize {
        self.text
            .char_indices()
            .take_while(|(index, _)| *index < self.cursor)
            .last()
            .map_or(0, |(index, _)| index)
    }

    /// Finds the next valid UTF-8 character boundary after the cursor.
    fn next_boundary(&self) -> usize {
        if self.cursor == self.text.len() {
            return self.cursor;
        }

        // Walk global character boundaries instead of slicing at the cursor so
        // the clippy string-slice lint does not need a local exception.
        self.text
            .char_indices()
            .find(|(index, _)| *index > self.cursor)
            .map_or(self.text.len(), |(index, _)| index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    #[test]
    fn composer_inserts_plain_characters_at_cursor() {
        let mut composer = Composer::default();

        assert_eq!(
            composer.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
            ComposerAction::Redraw
        );
        assert_eq!(
            composer.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE)),
            ComposerAction::Redraw
        );

        assert_eq!(composer.text(), "hi");
        assert_eq!(composer.cursor(), 2);
    }

    #[test]
    fn composer_submits_and_clears_on_enter() {
        let mut composer = Composer::default();
        composer.insert_str("hello");

        let action = composer.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, ComposerAction::Submit("hello".to_string()));
        assert!(composer.is_empty());
        assert_eq!(composer.cursor(), 0);
    }

    #[test]
    fn composer_ctrl_j_inserts_newline() {
        let mut composer = Composer::default();
        composer.insert_str("a");

        let action = composer.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "a\n");
    }

    #[test]
    fn composer_backspace_removes_previous_character() {
        let mut composer = Composer::default();
        composer.insert_str("abc");
        composer.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        assert_eq!(
            composer.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
            ComposerAction::Redraw
        );
        assert_eq!(composer.text(), "ac");
        assert_eq!(composer.cursor(), 1);
    }

    /// Verifies release events do not edit prompt text.
    #[test]
    fn composer_ignores_release_events_without_mutating_text() {
        let mut composer = Composer::default();
        composer.insert_str("a");

        let action = composer.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('b'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));

        assert_eq!(action, ComposerAction::Ignored);
        assert_eq!(composer.text(), "a");
        assert_eq!(composer.cursor(), 1);
    }

    /// Verifies backspace deletes one UTF-8 character before the cursor.
    #[test]
    fn composer_backspace_deletes_utf8_character_before_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("a你b");
        composer.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        let action = composer.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "ab");
        assert_eq!(composer.cursor(), 1);
    }

    /// Verifies delete removes one UTF-8 character at the cursor.
    #[test]
    fn composer_delete_removes_utf8_character_at_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("a你b");
        composer.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        composer.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        let action = composer.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "ab");
        assert_eq!(composer.cursor(), 1);
    }
}
