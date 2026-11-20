//! Built-in theme registration and appearance resolution.

use desktop::state::{AppearancePreference, ThemeModePreference};
use desktop::theme::register_builtin_themes;
use gpui_kit::component::{ActiveTheme as _, ThemeRegistry};
use gpui_kit::{TestAppContext, WindowAppearance};

/// Initializes gpui-kit and registers the vendored themes.
fn setup(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(register_builtin_themes);
}

#[gpui_kit::test]
fn registers_all_builtin_themes(cx: &mut TestAppContext) {
    setup(cx);
    cx.update(|cx| {
        let themes = ThemeRegistry::global(cx).themes();
        assert_eq!(themes.len(), 38);
        for name in [
            "Catppuccin Mocha",
            "Catppuccin Latte",
            "Tokyo Night",
            "Gruvbox Light",
            "macOS Classic Dark",
        ] {
            assert!(themes.contains_key(name), "{name} missing");
        }
    });
}

#[gpui_kit::test]
fn dark_preference_applies_selected_dark_theme(cx: &mut TestAppContext) {
    setup(cx);
    cx.update(|cx| {
        AppearancePreference::builder()
            .mode(ThemeModePreference::Dark)
            .dark_theme("Catppuccin Mocha".to_string())
            .build()
            .apply(WindowAppearance::Light, cx);
        assert_eq!(cx.theme().theme_name().as_ref(), "Catppuccin Mocha");
        assert!(cx.theme().is_dark());
    });
}

#[gpui_kit::test]
fn system_mode_follows_window_appearance(cx: &mut TestAppContext) {
    setup(cx);
    cx.update(|cx| {
        let appearance = AppearancePreference::builder()
            .light_theme("Ayu Light".to_string())
            .build();
        appearance.apply(WindowAppearance::Light, cx);
        assert_eq!(cx.theme().theme_name().as_ref(), "Ayu Light");
        appearance.apply(WindowAppearance::VibrantDark, cx);
        assert_eq!(cx.theme().theme_name().as_ref(), "Default Dark");
    });
}

#[gpui_kit::test]
fn unknown_or_wrong_mode_theme_falls_back(cx: &mut TestAppContext) {
    setup(cx);
    cx.update(|cx| {
        let dark = |name: &str| {
            AppearancePreference::builder()
                .mode(ThemeModePreference::Dark)
                .dark_theme(name.to_string())
                .build()
        };
        dark("Nope").apply(WindowAppearance::Dark, cx);
        assert_eq!(cx.theme().theme_name().as_ref(), "Default Dark");
        dark("Catppuccin Latte").apply(WindowAppearance::Dark, cx);
        assert_eq!(cx.theme().theme_name().as_ref(), "Default Dark");
    });
}

#[gpui_kit::test]
fn font_size_is_clamped(cx: &mut TestAppContext) {
    setup(cx);
    cx.update(|cx| {
        AppearancePreference::builder()
            .font_size(40.0)
            .build()
            .apply(WindowAppearance::Light, cx);
        assert_eq!(f32::from(cx.theme().font_size), 20.0);
        AppearancePreference::builder()
            .font_size(2.0)
            .build()
            .apply(WindowAppearance::Light, cx);
        assert_eq!(f32::from(cx.theme().font_size), 12.0);
    });
}
