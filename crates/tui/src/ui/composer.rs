//! Editable prompt composer state.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_width::UnicodeWidthChar;

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

    /// Returns the cursor cell offset from the start of the composer area.
    pub fn cursor_cell_offset(
        &self,
        available_width: u16,
        prompt_prefix_width: u16,
    ) -> (u16, u16) {
        let width = usize::from(available_width.max(1));
        let mut row = 0usize;
        let mut column = usize::from(prompt_prefix_width);

        // The composer is rendered with a prompt prefix on the first visual
        // row only, so explicit newlines reset to column zero.
        for ch in self
            .text
            .char_indices()
            .take_while(|(index, _)| *index < self.cursor)
            .map(|(_, ch)| ch)
        {
            if ch == '\n' {
                row = row.saturating_add(1);
                column = 0;
                continue;
            }

            let char_width = ch.width().unwrap_or(0);
            if char_width == 0 {
                continue;
            }
            if column.saturating_add(char_width) > width {
                row = row.saturating_add(1);
                column = 0;
            }
            column = column.saturating_add(char_width);
            if column >= width {
                row = row.saturating_add(1);
                column = 0;
            }
        }

        (column as u16, row as u16)
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
            (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.cursor = 0;
                ComposerAction::Redraw
            }
            (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cursor = self.text.len();
                ComposerAction::Redraw
            }
            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                self.delete_word_before_cursor();
                ComposerAction::Redraw
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.move_left();
                ComposerAction::Redraw
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.move_right();
                ComposerAction::Redraw
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.delete_at_cursor();
                ComposerAction::Redraw
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.delete_before_cursor_to_start();
                ComposerAction::Redraw
            }
            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.delete_from_cursor_to_end();
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

    /// Deletes the word immediately before the cursor, skipping trailing spaces first.
    fn delete_word_before_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let mut delete_start = self.cursor;
        let mut seen_word = false;
        for (index, ch) in self
            .text
            .char_indices()
            .filter(|(index, _)| *index < self.cursor)
            .rev()
        {
            if ch.is_whitespace() {
                if seen_word {
                    break;
                }
            } else {
                seen_word = true;
            }
            delete_start = index;
        }

        self.text.drain(delete_start..self.cursor);
        self.cursor = delete_start;
    }

    /// Deletes all prompt text before the cursor.
    fn delete_before_cursor_to_start(&mut self) {
        self.text.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Deletes all prompt text after the cursor.
    fn delete_from_cursor_to_end(&mut self) {
        self.text.truncate(self.cursor);
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
            composer.handle_key(KeyEvent::new(
                KeyCode::Char('h'),
                KeyModifiers::NONE
            )),
            ComposerAction::Redraw
        );
        assert_eq!(
            composer.handle_key(KeyEvent::new(
                KeyCode::Char('i'),
                KeyModifiers::NONE
            )),
            ComposerAction::Redraw
        );

        assert_eq!(composer.text(), "hi");
        assert_eq!(composer.cursor(), 2);
    }

    #[test]
    fn composer_submits_and_clears_on_enter() {
        let mut composer = Composer::default();
        composer.insert_str("hello");

        let action = composer
            .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(action, ComposerAction::Submit("hello".to_string()));
        assert!(composer.is_empty());
        assert_eq!(composer.cursor(), 0);
    }

    #[test]
    fn composer_ctrl_j_inserts_newline() {
        let mut composer = Composer::default();
        composer.insert_str("a");

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "a\n");
    }

    /// Verifies Ctrl+A moves the cursor to the start of the prompt.
    #[test]
    fn composer_ctrl_a_moves_cursor_to_start() {
        let mut composer = Composer::default();
        composer.insert_str("hello");

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.cursor(), 0);
    }

    /// Verifies Ctrl+E moves the cursor to the end of the prompt.
    #[test]
    fn composer_ctrl_e_moves_cursor_to_end() {
        let mut composer = Composer::default();
        composer.insert_str("hello");
        composer.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.cursor(), composer.text().len());
    }

    /// Verifies Ctrl+W deletes the previous word and leaves preceding text intact.
    #[test]
    fn composer_ctrl_w_deletes_word_before_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("hello   world");

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "hello   ");
        assert_eq!(composer.cursor(), "hello   ".len());
    }

    /// Verifies Ctrl+W skips whitespace before deleting the previous word.
    #[test]
    fn composer_ctrl_w_skips_whitespace_before_word() {
        let mut composer = Composer::default();
        composer.insert_str("hello world   ");

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "hello ");
        assert_eq!(composer.cursor(), "hello ".len());
    }

    /// Verifies Ctrl+B and Ctrl+F move the cursor by one character.
    #[test]
    fn composer_ctrl_b_and_ctrl_f_move_by_character() {
        let mut composer = Composer::default();
        composer.insert_str("a你b");

        assert_eq!(
            composer.handle_key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL
            )),
            ComposerAction::Redraw
        );
        assert_eq!(composer.cursor(), "a你".len());

        assert_eq!(
            composer.handle_key(KeyEvent::new(
                KeyCode::Char('f'),
                KeyModifiers::CONTROL
            )),
            ComposerAction::Redraw
        );
        assert_eq!(composer.cursor(), "a你b".len());
    }

    /// Verifies Ctrl+D deletes one UTF-8 character at the cursor.
    #[test]
    fn composer_ctrl_d_deletes_character_at_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("a你b");
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        ));
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ));

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "ab");
        assert_eq!(composer.cursor(), 1);
    }

    /// Verifies Ctrl+U deletes text before the cursor.
    #[test]
    fn composer_ctrl_u_deletes_before_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("hello world");
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL,
        ));

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "ld");
        assert_eq!(composer.cursor(), 0);
    }

    /// Verifies Ctrl+K deletes text after the cursor.
    #[test]
    fn composer_ctrl_k_deletes_after_cursor() {
        let mut composer = Composer::default();
        composer.insert_str("hello world");
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        ));
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ));
        composer.handle_key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ));

        let action = composer.handle_key(KeyEvent::new(
            KeyCode::Char('k'),
            KeyModifiers::CONTROL,
        ));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "he");
        assert_eq!(composer.cursor(), 2);
    }

    #[test]
    fn composer_backspace_removes_previous_character() {
        let mut composer = Composer::default();
        composer.insert_str("abc");
        composer.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));

        assert_eq!(
            composer.handle_key(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::NONE
            )),
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

        let action = composer
            .handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));

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

        let action = composer
            .handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

        assert_eq!(action, ComposerAction::Redraw);
        assert_eq!(composer.text(), "ab");
        assert_eq!(composer.cursor(), 1);
    }
}
