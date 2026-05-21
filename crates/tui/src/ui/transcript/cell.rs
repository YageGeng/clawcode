//! Trait-object transcript cells for TUI rendering.

use std::any::Any;

use ratatui::text::Line;

use crate::ui::theme::Theme;

/// Render mode for transcript cells.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptRenderMode {
    /// Rich terminal rendering.
    Rich,
    /// Copy-friendly plain rendering.
    Raw,
}

/// A single renderable unit in the TUI transcript.
pub trait TranscriptCell: std::fmt::Debug + Send + Sync + Any {
    /// Returns logical lines for the main rich transcript view.
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>>;

    /// Returns copy-friendly plain logical lines.
    fn raw_lines(&self) -> Vec<Line<'static>>;

    /// Returns logical lines for one render mode.
    fn display_lines_for_mode(
        &self,
        width: u16,
        theme: &Theme,
        mode: TranscriptRenderMode,
    ) -> Vec<Line<'static>> {
        match mode {
            TranscriptRenderMode::Rich => self.display_lines(width, theme),
            TranscriptRenderMode::Raw => self.raw_lines(),
        }
    }

    /// Returns viewport rows needed by rich display lines.
    fn desired_height(&self, width: u16, theme: &Theme) -> u16 {
        self.desired_height_for_mode(width, theme, TranscriptRenderMode::Rich)
    }

    /// Returns viewport rows needed by one render mode.
    fn desired_height_for_mode(
        &self,
        width: u16,
        theme: &Theme,
        mode: TranscriptRenderMode,
    ) -> u16 {
        display_height(self.display_lines_for_mode(width, theme, mode), width)
    }

    /// Returns logical lines for transcript cache rendering.
    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        self.display_lines(width, theme)
    }

    /// Returns viewport rows needed by transcript lines.
    fn desired_transcript_height(&self, width: u16, theme: &Theme) -> u16 {
        display_height(self.transcript_lines(width, theme), width)
    }

    /// Returns whether this cell continues a previous stream segment.
    fn is_stream_continuation(&self) -> bool {
        false
    }

    /// Returns a coarse cache key tick for animated transcript output.
    fn transcript_animation_tick(&self) -> Option<u64> {
        None
    }
}

/// Counts terminal rows for logical lines without depending on ratatui unstable APIs.
fn display_height(lines: Vec<Line<'static>>, width: u16) -> u16 {
    let count = if width == 0 {
        lines.len()
    } else {
        let width = usize::from(width);
        lines
            .iter()
            // Empty logical lines still occupy one terminal row.
            .map(|line| line.width().max(1).div_ceil(width))
            .sum()
    };
    count.try_into().unwrap_or(u16::MAX)
}

impl dyn TranscriptCell {
    /// Returns this cell as `Any` for type-specific state updates.
    pub fn as_any(&self) -> &dyn Any {
        self
    }
}
