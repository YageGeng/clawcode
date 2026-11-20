//! Built-in gpui-kit themes and how the appearance preference selects one.

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, WindowAppearance, px};

use crate::state::{AppearancePreference, ThemeModePreference};

/// Theme files vendored from gpui-kit `v0.10.0`, in file-name order.
pub const BUILTIN_THEMES: [&str; 21] = [
    include_str!("../themes/adventure.json"),
    include_str!("../themes/alduin.json"),
    include_str!("../themes/asciinema.json"),
    include_str!("../themes/aurora.json"),
    include_str!("../themes/ayu.json"),
    include_str!("../themes/catppuccin.json"),
    include_str!("../themes/everforest.json"),
    include_str!("../themes/fahrenheit.json"),
    include_str!("../themes/flexoki.json"),
    include_str!("../themes/gruvbox.json"),
    include_str!("../themes/harper.json"),
    include_str!("../themes/hybrid.json"),
    include_str!("../themes/jellybeans.json"),
    include_str!("../themes/kibble.json"),
    include_str!("../themes/macos-classic.json"),
    include_str!("../themes/mellifluous.json"),
    include_str!("../themes/molokai.json"),
    include_str!("../themes/solarized.json"),
    include_str!("../themes/spaceduck.json"),
    include_str!("../themes/tokyonight.json"),
    include_str!("../themes/twilight.json"),
];

/// Theme used in light mode when the preferred one is unavailable.
pub const DEFAULT_LIGHT_THEME: &str = "Default Light";
/// Theme used in dark mode when the preferred one is unavailable.
pub const DEFAULT_DARK_THEME: &str = "Default Dark";

/// Smallest and largest base font sizes the appearance setting accepts.
const FONT_SIZE_RANGE: (f32, f32) = (12.0, 20.0);

/// Registers every vendored theme; a file that fails to parse is logged and skipped.
pub fn register_builtin_themes(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    for theme in BUILTIN_THEMES {
        if let Err(error) = registry.load_themes_from_str(theme) {
            tracing::error!("failed to register a built-in theme: {}", error);
        }
    }
}

impl ThemeModePreference {
    /// Returns the mode to show, taking `System` from the window appearance.
    pub fn resolve(self, appearance: WindowAppearance) -> ThemeMode {
        match self {
            ThemeModePreference::Light => ThemeMode::Light,
            ThemeModePreference::Dark => ThemeMode::Dark,
            ThemeModePreference::System => match appearance {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => {
                    ThemeMode::Dark
                }
                WindowAppearance::Light | WindowAppearance::VibrantLight => {
                    ThemeMode::Light
                }
            },
        }
    }
}

impl AppearancePreference {
    /// Installs the preferred light and dark themes and the font size, then shows the resolved mode.
    ///
    /// A theme name that is not registered, or that belongs to the other mode,
    /// falls back to that mode's default theme.
    pub fn apply(&self, appearance: WindowAppearance, cx: &mut App) {
        let registry = ThemeRegistry::global(cx);
        let pick = |name: &str, mode: ThemeMode, fallback: &str| {
            let themes = registry.themes();
            themes
                .get(name)
                .filter(|theme| theme.mode == mode)
                .or_else(|| themes.get(fallback))
                .cloned()
        };
        let light =
            pick(&self.light_theme, ThemeMode::Light, DEFAULT_LIGHT_THEME);
        let dark = pick(&self.dark_theme, ThemeMode::Dark, DEFAULT_DARK_THEME);
        // A NaN font size compares false everywhere, so give it the default before clamping.
        let font_size = if self.font_size.is_nan() {
            AppearancePreference::default().font_size
        } else {
            self.font_size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1)
        };
        Theme::update(cx, |theme| {
            if let Some(light) = light {
                theme.light_theme = light;
            }
            if let Some(dark) = dark {
                theme.dark_theme = dark;
            }
        });
        Theme::change(self.mode.resolve(appearance), None, cx);
        // Changing the mode reloads the theme file, so the font size goes in afterwards.
        Theme::update(cx, |theme| theme.font_size = px(font_size));
    }
}
