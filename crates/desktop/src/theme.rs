//! Built-in gpui-kit themes and how the appearance preference selects one.

use std::sync::Arc;

use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, FontWeight, Global, SharedString, WindowAppearance, px};

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

/// The interface and code font weights the settings chose; read with [`FontWeights::get`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FontWeights {
    /// Weight of interface text, set on the window's root element.
    pub ui: FontWeight,
    /// Weight of monospace text, set on code containers.
    pub code: FontWeight,
}

impl Global for FontWeights {}

impl Default for FontWeights {
    /// Normal weight for both.
    fn default() -> Self {
        Self {
            ui: FontWeight::NORMAL,
            code: FontWeight::NORMAL,
        }
    }
}

impl FontWeights {
    /// The weights last applied, or normal weight before any appearance was applied.
    pub fn get(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_default()
    }
}

/// The installed font families, listed once per run; read with [`InstalledFonts::get`].
#[derive(Debug, Clone)]
pub struct InstalledFonts(Arc<[String]>);

impl Global for InstalledFonts {}

impl InstalledFonts {
    /// The installed families, asking the text system only the first time.
    // ponytail: fonts installed while the app runs show up after a restart.
    pub fn get(cx: &mut App) -> Arc<[String]> {
        if let Some(fonts) = cx.try_global::<Self>() {
            return Arc::clone(&fonts.0);
        }
        let fonts: Arc<[String]> = cx.text_system().all_font_names().into();
        cx.set_global(Self(Arc::clone(&fonts)));
        fonts
    }
}

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
    /// Installs the preferred light and dark themes, shows the resolved mode,
    /// then applies the font settings on top of the theme's fonts.
    ///
    /// A theme name that is not registered, or that belongs to the other mode,
    /// falls back to that mode's default theme.
    pub fn apply(&self, appearance: WindowAppearance, cx: &mut App) {
        let installed = InstalledFonts::get(cx);
        self.apply_with_fonts(appearance, &installed, cx);
    }

    /// [`Self::apply`] against an explicit list of installed font families.
    pub fn apply_with_fonts(
        &self,
        appearance: WindowAppearance,
        installed: &[String],
        cx: &mut App,
    ) {
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
        let clamped = self.clamped();
        // Saved fonts that are not installed keep the theme's own fonts.
        let font = |name: &Option<String>| {
            name.as_ref()
                .filter(|name| installed.contains(name))
                .map(|name| SharedString::from(name.clone()))
        };
        let (ui_font, code_font) = (font(&self.ui_font), font(&self.code_font));
        Theme::update(cx, |theme| {
            if let Some(light) = light {
                theme.light_theme = light;
            }
            if let Some(dark) = dark {
                theme.dark_theme = dark;
            }
        });
        Theme::change(self.mode.resolve(appearance), None, cx);
        // Changing the mode reloads the theme file, so fonts go in afterwards.
        Theme::update(cx, |theme| {
            theme.font_size = px(clamped.font_size);
            theme.mono_font_size = px(clamped.code_font_size);
            if let Some(family) = ui_font {
                theme.font_family = family;
            }
            if let Some(family) = code_font {
                theme.mono_font_family = family;
            }
        });
        cx.set_global(FontWeights {
            ui: FontWeight(f32::from(clamped.ui_font_weight)),
            code: FontWeight(f32::from(clamped.code_font_weight)),
        });
    }
}
