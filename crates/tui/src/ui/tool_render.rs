//! Tool-call rendering helpers for the local TUI.

use agent_client_protocol::schema::ToolCallStatus;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::ui::state::ToolCallView;
use crate::ui::terminal_output::terminal_display_lines;
use crate::ui::tool_summary::tool_summary;

const TOOL_OUTPUT_PREVIEW_LINES: usize = 5;

/// Renders one tool-call transcript cell with a Codex-style compact preview.
pub(super) fn append_tool_call_lines(lines: &mut Vec<Line<'static>>, call: &ToolCallView) {
    lines.push(Line::from(vec![
        status_bullet(call.status()),
        " ".into(),
        Span::styled(
            format!("{} {}", status_verb(call.status()), tool_summary(call)),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    append_tool_output_preview_lines(lines, call.status(), call.output());
}

/// Appends the first five display lines from normalized tool output.
fn append_tool_output_preview_lines(
    lines: &mut Vec<Line<'static>>,
    status: ToolCallStatus,
    text: &str,
) {
    let display_lines = terminal_display_lines(text);
    if display_lines.is_empty() {
        if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
            lines.push(dim_line("  └ (no output)"));
        }
        return;
    }

    for (index, line) in display_lines
        .iter()
        .take(TOOL_OUTPUT_PREVIEW_LINES)
        .enumerate()
    {
        let prefix = if index == 0 { "  └ " } else { "    " };
        lines.push(dim_line(format!("{prefix}{line}")));
    }

    if display_lines.len() > TOOL_OUTPUT_PREVIEW_LINES {
        let omitted = display_lines.len() - TOOL_OUTPUT_PREVIEW_LINES;
        lines.push(dim_line(format!("    ... +{omitted} lines")));
    }
}

/// Builds a dimmed display line for secondary tool output.
fn dim_line(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().add_modifier(Modifier::DIM),
    ))
}

/// Returns the status verb shown in the tool-call header.
fn status_verb(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "Queued",
        ToolCallStatus::InProgress => "Running",
        ToolCallStatus::Completed => "Ran",
        ToolCallStatus::Failed => "Failed",
        _ => "Tool",
    }
}

/// Returns the bullet style shown in the tool-call header.
fn status_bullet(status: ToolCallStatus) -> Span<'static> {
    let style = match status {
        ToolCallStatus::Completed => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        ToolCallStatus::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().add_modifier(Modifier::DIM),
    };
    Span::styled("•", style)
}
