//! Semantic terminal theme tokens for the local TUI.
//!
//! The base tokens stay intentionally broad. Component-specific colors such as
//! diff rows are exposed as methods so renderers can keep readable names
//! without making every component state a first-class palette field.

use ratatui::style::Color;

/// Broad semantic color tokens used by TUI renderers.
#[derive(Debug, Clone, PartialEq, Eq, typed_builder::TypedBuilder)]
pub struct Theme {
    /// Default foreground text.
    pub text: Color,
    /// Secondary text such as hints, transcript metadata, and context rows.
    pub muted: Color,
    /// Primary accent for focused or identifying UI labels.
    pub accent: Color,
    /// Successful or completed status.
    pub success: Color,
    /// Warning or pending status.
    pub warning: Color,
    /// Error, failed, or destructive status.
    pub danger: Color,
    /// Optional surface background for input or overlay regions.
    pub surface: Color,
    /// Border and separator color.
    pub border: Color,
}

impl Theme {
    /// Returns the terminal-adaptive default theme.
    #[must_use]
    pub fn dark() -> Self {
        Self::builder()
            .text(Color::Reset)
            .muted(Color::DarkGray)
            .accent(Color::Cyan)
            .success(Color::Green)
            .warning(Color::Yellow)
            .danger(Color::Red)
            .surface(Color::Reset)
            .border(Color::DarkGray)
            .build()
    }

    /// Returns the terminal-adaptive light theme.
    #[must_use]
    pub fn light() -> Self {
        Self::builder()
            .text(Color::Reset)
            .muted(Color::DarkGray)
            .accent(Color::Blue)
            .success(Color::Green)
            .warning(Color::Yellow)
            .danger(Color::Red)
            .surface(Color::Reset)
            .border(Color::DarkGray)
            .build()
    }

    /// Builds a render theme from the file-backed TUI configuration.
    #[must_use]
    pub fn from_config(theme: config::TuiTheme) -> Self {
        match theme {
            config::TuiTheme::Dark => Self::dark(),
            config::TuiTheme::Light => Self::light(),
        }
    }

    /// Returns the color for added lines in unified diff output.
    #[must_use]
    pub fn diff_added(&self) -> Color {
        self.success
    }

    /// Returns the color for removed lines in unified diff output.
    #[must_use]
    pub fn diff_removed(&self) -> Color {
        self.danger
    }

    /// Returns the color for diff headers and context rows.
    #[must_use]
    pub fn diff_context(&self) -> Color {
        self.muted
    }

    /// Returns the color for the model/provider label in the status row.
    #[must_use]
    pub fn model_label(&self) -> Color {
        self.accent
    }

    /// Returns the color for the working-directory label in the status row.
    #[must_use]
    pub fn cwd(&self) -> Color {
        self.success
    }

    /// Returns the input composer background color.
    #[must_use]
    pub fn composer_bg(&self) -> Color {
        self.surface
    }
}

impl Default for Theme {
    /// Returns the default render theme.
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies diff colors are derived from broader semantic tokens.
    #[test]
    fn diff_colors_come_from_semantic_status_tokens() {
        let theme = Theme::dark();

        assert_eq!(theme.diff_added(), theme.success);
        assert_eq!(theme.diff_removed(), theme.danger);
    }

    /// Verifies config theme variants map to render themes.
    #[test]
    fn theme_maps_from_config_theme() {
        assert_eq!(Theme::from_config(config::TuiTheme::Light), Theme::light());
    }
}
