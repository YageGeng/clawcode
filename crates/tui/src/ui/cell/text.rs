//! Text transcript cells for the local TUI.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Role-specific styling for text transcript cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextRole {
    /// Assistant answer text.
    Assistant,
    /// Assistant reasoning text.
    Reasoning,
    /// User prompt text.
    User,
    /// System or runtime text.
    System,
}

/// Renderable text transcript cell.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct TextCell {
    /// Role that controls display prefix and style.
    role: TextRole,
    /// Text accumulated for this transcript cell.
    text: String,
}

impl TextCell {
    /// Creates a text cell for one transcript role.
    pub fn new(role: TextRole, text: impl Into<String>) -> Self {
        Self::builder().role(role).text(text.into()).build()
    }

    /// Returns the role that controls display behavior.
    pub fn role(&self) -> TextRole {
        self.role
    }

    /// Returns the stored text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Appends streaming text to this cell.
    pub fn push_str(&mut self, text: &str) {
        self.text.push_str(text);
    }

    /// Returns styled logical lines for this text cell.
    pub fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let (first_prefix, style) = match self.role {
            TextRole::Assistant => ("", Style::default().fg(Color::Reset)),
            TextRole::Reasoning => (
                "",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            TextRole::User => ("> ", Style::default().add_modifier(Modifier::BOLD)),
            TextRole::System => ("system: ", Style::default().fg(Color::DarkGray)),
        };
        styled_text_lines(&self.text, first_prefix, style)
    }

    /// Returns plain logical lines suitable for copy/raw transcript modes.
    pub fn raw_lines(&self) -> Vec<Line<'static>> {
        self.text
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect()
    }
}

/// Builds one styled line per newline-delimited segment.
fn styled_text_lines(text: &str, first_prefix: &str, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut added = false;
    for (index, segment) in text.split('\n').enumerate() {
        let prefix = if index == 0 { first_prefix } else { "" };
        lines.push(Line::from(Span::styled(
            format!("{prefix}{segment}"),
            style,
        )));
        added = true;
    }

    if !added {
        lines.push(Line::from(Span::styled(first_prefix.to_string(), style)));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies reasoning output keeps the distinct low-emphasis style.
    #[test]
    fn text_cell_reasoning_lines_use_distinct_style() {
        let cell = TextCell::new(TextRole::Reasoning, "thinking");

        let lines = cell.display_lines(80);

        let span = lines[0].spans.first().expect("reasoning span");
        assert_eq!(span.content, "thinking");
        assert_eq!(span.style.fg, Some(Color::DarkGray));
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    /// Verifies user text keeps the prompt prefix on the first logical line only.
    #[test]
    fn text_cell_user_lines_prefix_first_line_only() {
        let cell = TextCell::new(TextRole::User, "hello\nworld");

        let lines = cell.display_lines(80);

        assert_eq!(lines[0].spans[0].content, "> hello");
        assert_eq!(lines[1].spans[0].content, "world");
    }
}
