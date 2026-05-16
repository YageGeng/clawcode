//! Terminal output normalization for the local TUI.

/// Converts captured terminal output into stable text lines for ratatui rendering.
pub(super) fn terminal_display_lines(text: &str) -> Vec<String> {
    let text = strip_ansi_control_sequences(text);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if matches!(chars.peek(), Some('\n')) {
                    let _ = chars.next();
                    lines.push(std::mem::take(&mut current));
                } else {
                    // Carriage return rewrites the current terminal row; keep only the final state.
                    current.clear();
                }
            }
            '\n' => {
                lines.push(std::mem::take(&mut current));
            }
            '\t' => current.push('\t'),
            ch if ch.is_control() => {}
            ch => current.push(ch),
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    lines
}

/// Removes common ANSI escape/control sequences from captured command output.
fn strip_ansi_control_sequences(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            stripped.push(ch);
            continue;
        }

        // Consume a basic ANSI escape sequence through its final byte.
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }

    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies carriage-return progress output keeps the final terminal row state.
    #[test]
    fn terminal_display_lines_handles_carriage_return_updates() {
        assert_eq!(
            terminal_display_lines("progress 10%\rprogress 100%\ndone"),
            vec!["progress 100%".to_string(), "done".to_string()]
        );
    }
}
