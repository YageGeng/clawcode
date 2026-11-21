//! Desktop-only UI state persisted in `desktop.json`, separate from backend data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use protocol::ProductIdentity;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::file::icons::Flavor;
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
    /// File icons used in light mode.
    #[builder(default)]
    pub light_file_icons: FileIconTheme,
    /// File icons used in dark mode.
    #[builder(default)]
    pub dark_file_icons: FileIconTheme,
    /// Interface font family; `None` keeps the theme's font.
    #[builder(default)]
    pub ui_font: Option<String>,
    /// Interface font weight, 100–900.
    #[builder(default = 400)]
    pub ui_font_weight: u16,
    /// Code font family; `None` keeps the theme's monospace font.
    #[builder(default)]
    pub code_font: Option<String>,
    /// Code font size in pixels.
    #[builder(default = 14.0)]
    pub code_font_size: f32,
    /// Code font weight, 100–900.
    #[builder(default = 400)]
    pub code_font_weight: u16,
}

impl Default for AppearancePreference {
    /// Follows the system with the two default themes, 16px interface and 14px code text.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl AppearancePreference {
    /// Interface font sizes the settings accept, in pixels.
    pub const UI_FONT_SIZE_RANGE: (f32, f32) = (12.0, 20.0);
    /// Code font sizes the settings accept, in pixels.
    pub const CODE_FONT_SIZE_RANGE: (f32, f32) = (10.0, 24.0);
    /// Font weights the settings accept.
    pub const FONT_WEIGHT_RANGE: (u16, u16) = (100, 900);

    /// The font weight nearest to a typed number, inside [`Self::FONT_WEIGHT_RANGE`].
    pub fn font_weight_from(value: f32) -> u16 {
        let (low, high) = Self::FONT_WEIGHT_RANGE;
        // Searching the range avoids a lossy float-to-integer cast.
        (low..=high)
            .min_by(|a, b| {
                (f32::from(*a) - value)
                    .abs()
                    .total_cmp(&(f32::from(*b) - value).abs())
            })
            .unwrap_or(400)
    }

    /// A copy with every number inside its range; a NaN size takes its default.
    pub fn clamped(&self) -> Self {
        let defaults = Self::default();
        let size = |value: f32, default: f32, (low, high): (f32, f32)| {
            if value.is_nan() {
                default
            } else {
                value.clamp(low, high)
            }
        };
        let (low, high) = Self::FONT_WEIGHT_RANGE;
        Self {
            font_size: size(
                self.font_size,
                defaults.font_size,
                Self::UI_FONT_SIZE_RANGE,
            ),
            code_font_size: size(
                self.code_font_size,
                defaults.code_font_size,
                Self::CODE_FONT_SIZE_RANGE,
            ),
            ui_font_weight: self.ui_font_weight.clamp(low, high),
            code_font_weight: self.code_font_weight.clamp(low, high),
            ..self.clone()
        }
    }
}

/// Which icons the file tree and file tabs use; unknown stored values read back as `Default`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FileIconTheme {
    /// Catppuccin's colored icons in the Latte flavor.
    CatppuccinLatte,
    /// Catppuccin's colored icons in the Frappé flavor.
    CatppuccinFrappe,
    /// Catppuccin's colored icons in the Macchiato flavor.
    CatppuccinMacchiato,
    /// Catppuccin's colored icons in the Mocha flavor.
    CatppuccinMocha,
    /// The interface's own Lucide file and folder icons; serde requires the catch-all variant last.
    #[default]
    #[serde(other)]
    Default,
}

impl FileIconTheme {
    /// Every choice, in the order the settings list them.
    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::CatppuccinLatte,
        Self::CatppuccinFrappe,
        Self::CatppuccinMacchiato,
        Self::CatppuccinMocha,
    ];

    /// The Catppuccin flavor this theme draws in; `None` for the Lucide icons.
    pub fn flavor(self) -> Option<Flavor> {
        match self {
            Self::Default => None,
            Self::CatppuccinLatte => Some(Flavor::Latte),
            Self::CatppuccinFrappe => Some(Flavor::Frappe),
            Self::CatppuccinMacchiato => Some(Flavor::Macchiato),
            Self::CatppuccinMocha => Some(Flavor::Mocha),
        }
    }

    /// The theme saved under `key`, if it names one.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.key() == key)
    }

    /// The value saved in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::CatppuccinLatte => "catppuccin_latte",
            Self::CatppuccinFrappe => "catppuccin_frappe",
            Self::CatppuccinMacchiato => "catppuccin_macchiato",
            Self::CatppuccinMocha => "catppuccin_mocha",
        }
    }
}

/// Window and sidebar geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(default)]
pub struct LayoutState {
    /// Whether the sidebar is shown.
    #[builder(default = true)]
    pub sidebar_open: bool,
    /// Sidebar width in pixels as last dragged; read through [`Self::sidebar_width`].
    #[builder(default = LayoutState::DEFAULT_SIDEBAR_WIDTH)]
    pub sidebar_width: f32,
    /// Last window size in pixels.
    #[builder(default)]
    pub window_size: Option<(f32, f32)>,
    /// Right panel width in pixels as last dragged; read through [`Self::panel_width`].
    #[builder(default = LayoutState::DEFAULT_PANEL_WIDTH)]
    pub panel_width: f32,
    /// Whether the session details card shows when it fits inline.
    #[builder(default = true)]
    pub details_open: bool,
}

impl LayoutState {
    /// Narrowest sidebar that keeps its labels usable.
    pub const MIN_SIDEBAR_WIDTH: f32 = 200.0;
    /// Default sidebar width.
    pub const DEFAULT_SIDEBAR_WIDTH: f32 = 248.0;
    /// Widest sidebar before it competes with the work area.
    pub const MAX_SIDEBAR_WIDTH: f32 = 400.0;

    /// Narrowest right panel that keeps a diff readable.
    pub const MIN_PANEL_WIDTH: f32 = 320.0;
    /// Default right panel width.
    pub const DEFAULT_PANEL_WIDTH: f32 = 480.0;

    /// The stored right panel width, at least the minimum; the layout caps it by the window.
    pub fn panel_width(&self) -> f32 {
        if self.panel_width.is_nan() {
            return Self::DEFAULT_PANEL_WIDTH;
        }
        self.panel_width.max(Self::MIN_PANEL_WIDTH)
    }

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
    /// Open sidebar and closed details at their default widths, no remembered window size.
    fn default() -> Self {
        Self::builder().build()
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

/// One tab of a right panel, as saved.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PanelTabKey {
    /// The workspace's changes.
    Changes,
    /// The workspace's file tree.
    Files,
    /// One file's preview.
    File {
        /// Root-relative path of the file.
        path: String,
    },
    /// A subagent session.
    Subagent {
        /// The subagent's session id.
        session: String,
    },
    /// A tab kind a newer build saved; dropped on load.
    #[serde(other)]
    Unknown,
}

/// One owner's right panel: whether it shows, its tabs, and when it was last used.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TypedBuilder,
)]
#[serde(default)]
pub struct PanelState {
    /// Whether the panel is shown.
    #[builder(default)]
    pub open: bool,
    /// Tabs in order.
    #[builder(default)]
    pub tabs: Vec<PanelTabKey>,
    /// Index of the active tab.
    #[builder(default)]
    pub active: Option<usize>,
    /// Whether the panel takes the main card's place.
    #[builder(default)]
    pub maximized: bool,
    /// Last use in Unix milliseconds; the oldest panels are forgotten first.
    #[builder(default)]
    pub last_used_ms: u64,
}

impl PanelState {
    /// Drops tabs this build does not know and keeps `active` inside the list.
    fn normalize(&mut self) {
        self.tabs.retain(|tab| *tab != PanelTabKey::Unknown);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(index) => Some(index.min(self.tabs.len() - 1)),
            None => None,
        };
    }
}

/// A row of the session details card the user can hide or show.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DetailsItem {
    /// Open the workspace in the file manager or an editor.
    OpenIn,
    /// The checked-out branch.
    Branch,
    /// The changed-lines summary.
    Changes,
    /// Parent session and subagents.
    Relations,
    /// Context window fill.
    Context,
    /// The last run's token usage.
    Usage,
    /// A row a newer build knows; dropped on load. Serde requires the catch-all variant last.
    #[serde(other)]
    Unknown,
}

impl DetailsItem {
    /// Every item, in the card's order.
    pub const ALL: [Self; 6] = [
        Self::OpenIn,
        Self::Branch,
        Self::Changes,
        Self::Relations,
        Self::Context,
        Self::Usage,
    ];

    /// Whether the item shows before the user chooses otherwise.
    pub fn default_visible(self) -> bool {
        !matches!(self, Self::Context | Self::Usage | Self::Unknown)
    }
}

/// Where "Open in" opens a workspace; an unknown saved value falls back to the file manager.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenIn {
    /// An external editor, by its command name.
    Editor(String),
    /// The system file manager.
    #[default]
    FileManager,
}

impl<'de> Deserialize<'de> for OpenIn {
    /// Reads `{"editor": name}`; anything else, including a kind a newer
    /// build wrote, opens the file manager rather than failing the file.
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        /// The two shapes a saved value can have.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved {
            /// An editor by name.
            Editor {
                /// The editor's command.
                editor: String,
            },
            /// Anything else.
            Other(serde::de::IgnoredAny),
        }
        Ok(match Saved::deserialize(deserializer)? {
            Saved::Editor { editor } => OpenIn::Editor(editor),
            Saved::Other(_) => OpenIn::FileManager,
        })
    }
}

/// Where a subagent opens on a plain click; a modified click takes the other place.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SubagentOpenTarget {
    /// In the main card, as a session tab.
    Main,
    /// In the right panel, beside the parent; serde requires the catch-all variant last.
    #[default]
    #[serde(other)]
    Panel,
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
    /// Right panels by owner key (`session:{id}` or `draft:{workspace}`).
    #[builder(default)]
    pub panels: BTreeMap<String, PanelState>,
    /// Details card rows whose visibility differs from the default.
    #[builder(default)]
    pub details_items: BTreeMap<DetailsItem, bool>,
    /// How "Open in" opens a workspace.
    #[builder(default)]
    pub open_in: OpenIn,
    /// Where a subagent opens on a plain click.
    #[builder(default)]
    pub subagent_open_target: SubagentOpenTarget,
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
    /// Most right panels remembered; the least recently used go first.
    pub const MAX_PANELS: usize = 50;

    /// Whether a details card row shows.
    pub fn details_item_visible(&self, item: DetailsItem) -> bool {
        self.details_items
            .get(&item)
            .copied()
            .unwrap_or_else(|| item.default_visible())
    }

    /// Shows or hides a details card row, storing only choices that differ
    /// from the default so a later default change reaches everyone else.
    pub fn set_details_item(&mut self, item: DetailsItem, visible: bool) {
        if visible == item.default_visible() {
            self.details_items.remove(&item);
        } else {
            self.details_items.insert(item, visible);
        }
    }

    /// Forgets panels whose owner `keep` rejects, then all but the
    /// [`Self::MAX_PANELS`] most recently used.
    pub fn prune_panels(&mut self, keep: impl Fn(&str) -> bool) {
        self.panels.retain(|key, _| keep(key));
        if self.panels.len() <= Self::MAX_PANELS {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self
            .panels
            .iter()
            .map(|(key, panel)| (panel.last_used_ms, key.clone()))
            .collect();
        by_age.sort();
        let excess = self.panels.len() - Self::MAX_PANELS;
        for (_, key) in by_age.into_iter().take(excess) {
            self.panels.remove(&key);
        }
    }

    /// Cleans saved panels — unknown tabs go, `active` stays inside the
    /// list — and drops details rows this build does not know.
    pub fn normalize_panels(&mut self) {
        for panel in self.panels.values_mut() {
            panel.normalize();
        }
        self.details_items.remove(&DetailsItem::Unknown);
    }

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
        match serde_json::from_str::<DesktopState>(&text) {
            Ok(mut state) => {
                state.normalize_panels();
                state
            }
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
