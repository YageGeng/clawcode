//! Desktop-only UI state persisted in `desktop.json`, separate from backend data.

use std::path::{Path, PathBuf};

use protocol::ProductIdentity;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::workspace::WorkspaceRegistry;

/// Interface language choice; unknown stored values read back as `System`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum LanguagePreference {
    /// English.
    #[serde(rename = "en")]
    En,
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    ZhCn,
    /// Follow the operating system locale; serde requires the catch-all variant last.
    #[default]
    #[serde(rename = "system", other)]
    System,
}

/// Light or dark choice; unknown stored values read back as `System`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ThemeModePreference {
    /// Always use the selected light theme.
    Light,
    /// Always use the selected dark theme.
    Dark,
    /// Follow the window appearance reported by the system; serde requires the catch-all variant last.
    #[default]
    #[serde(other)]
    System,
}

/// Appearance settings: mode, one theme per mode, and the base font size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(default)]
pub struct AppearancePreference {
    /// Which of the two themes applies.
    #[builder(default)]
    pub mode: ThemeModePreference,
    /// Registered theme name used in light mode.
    #[builder(default = String::from("Default Light"))]
    pub light_theme: String,
    /// Registered theme name used in dark mode.
    #[builder(default = String::from("Default Dark"))]
    pub dark_theme: String,
    /// Base font size in pixels; the theme's `rem`.
    #[builder(default = 16.0)]
    pub font_size: f32,
}

impl Default for AppearancePreference {
    /// Follows the system with the two default themes at 16px.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Window and sidebar geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutState {
    /// Whether the sidebar is shown.
    pub sidebar_open: bool,
    /// Sidebar width in pixels as last dragged; read through [`Self::sidebar_width`].
    pub sidebar_width: f32,
    /// Last window size in pixels.
    pub window_size: Option<(f32, f32)>,
}

impl LayoutState {
    /// Narrowest sidebar that keeps its labels usable.
    pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;
    /// Default sidebar width.
    pub const DEFAULT_SIDEBAR_WIDTH: f32 = 248.0;
    /// Widest sidebar before it competes with the work area.
    pub const MAX_SIDEBAR_WIDTH: f32 = 400.0;

    /// Returns the stored sidebar width clamped to its allowed range.
    pub fn sidebar_width(&self) -> f32 {
        if self.sidebar_width.is_nan() {
            return Self::DEFAULT_SIDEBAR_WIDTH;
        }
        self.sidebar_width
            .clamp(Self::MIN_SIDEBAR_WIDTH, Self::MAX_SIDEBAR_WIDTH)
    }
}

impl Default for LayoutState {
    /// Open sidebar at its default width, no remembered window size.
    fn default() -> Self {
        Self {
            sidebar_open: true,
            sidebar_width: Self::DEFAULT_SIDEBAR_WIDTH,
            window_size: None,
        }
    }
}

/// Session tabs open in the main card.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabsState {
    /// Session ids in tab order; draft tabs are not stored.
    pub open: Vec<String>,
    /// Index of the active tab in `open`.
    pub active: Option<usize>,
}

/// Everything the desktop remembers between launches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(default)]
pub struct DesktopState {
    /// File format version.
    #[builder(default = DesktopState::VERSION)]
    pub version: u32,
    /// Interface language.
    #[builder(default)]
    pub language: LanguagePreference,
    /// Themes and font size.
    #[builder(default)]
    pub appearance: AppearancePreference,
    /// Registered workspaces.
    #[builder(default)]
    pub workspaces: WorkspaceRegistry,
    /// Window and sidebar geometry.
    #[builder(default)]
    pub layout: LayoutState,
    /// Open session tabs.
    #[builder(default)]
    pub tabs: TabsState,
}

impl Default for DesktopState {
    /// A fresh state for the current file version.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl DesktopState {
    /// Current `desktop.json` format version.
    pub const VERSION: u32 = 1;

    /// Returns `<config dir>/clawcode/desktop.json`, when the platform has a config directory.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| {
            dir.join(ProductIdentity::CONFIG_DIR_NAME)
                .join("desktop.json")
        })
    }

    /// Loads the state file; a missing file gives defaults, a damaged one is kept as `.bak`.
    pub fn load(path: &Path) -> DesktopState {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return DesktopState::default();
            }
            Err(error) => {
                Self::set_aside(path, &error.to_string());
                return DesktopState::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(state) => state,
            Err(error) => {
                Self::set_aside(path, &error.to_string());
                DesktopState::default()
            }
        }
    }

    /// Writes the state atomically: a temporary sibling file renamed over the target.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json =
            serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, path)
    }

    /// Renames an unreadable state file to `<name>.bak` so defaults can be rebuilt.
    fn set_aside(path: &Path, reason: &str) {
        let backup = path.with_extension("json.bak");
        match std::fs::rename(path, &backup) {
            Ok(()) => tracing::warn!(
                "desktop state {} was unreadable ({}); kept it as {} and loaded defaults",
                path.display(),
                reason,
                backup.display()
            ),
            Err(error) => tracing::warn!(
                "desktop state {} was unreadable ({}) and could not be backed up: {}",
                path.display(),
                reason,
                error
            ),
        }
    }
}
