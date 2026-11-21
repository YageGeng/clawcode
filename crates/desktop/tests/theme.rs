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

#[gpui_kit::test]
fn fonts_apply_to_the_theme_and_survive_a_theme_switch(
    cx: &mut TestAppContext,
) {
    setup(cx);
    // The test platform has no fonts, so the installed list is given.
    let font = "Test Sans".to_string();
    let installed = [font.clone()];
    let mut appearance = AppearancePreference {
        mode: ThemeModePreference::Dark,
        ui_font: Some(font.clone()),
        code_font_size: 18.0,
        ..AppearancePreference::default()
    };
    cx.update(|cx| {
        appearance.apply_with_fonts(WindowAppearance::Dark, &installed, cx)
    });
    cx.update(|cx| {
        assert_eq!(cx.theme().mono_font_size, gpui_kit::px(18.));
        assert_eq!(cx.theme().font_family.as_ref(), font);
    });
    appearance.dark_theme = "Catppuccin Mocha".to_string();
    cx.update(|cx| {
        appearance.apply_with_fonts(WindowAppearance::Dark, &installed, cx)
    });
    cx.update(|cx| {
        assert_eq!(cx.theme().theme_name().as_ref(), "Catppuccin Mocha");
        assert_eq!(cx.theme().mono_font_size, gpui_kit::px(18.));
        assert_eq!(cx.theme().font_family.as_ref(), font);
    });
}

#[gpui_kit::test]
fn missing_font_keeps_the_theme_default_and_the_setting(
    cx: &mut TestAppContext,
) {
    setup(cx);
    let default_family =
        cx.update(|cx| cx.theme().font_family.clone().to_string());
    let appearance = AppearancePreference {
        ui_font: Some("No Such Font 123".to_string()),
        ..AppearancePreference::default()
    };
    cx.update(|cx| appearance.apply(WindowAppearance::Light, cx));
    cx.update(|cx| assert_eq!(cx.theme().font_family.as_ref(), default_family));
    assert_eq!(appearance.ui_font.as_deref(), Some("No Such Font 123"));
}

#[gpui_kit::test]
fn font_weights_are_global(cx: &mut TestAppContext) {
    use desktop::theme::FontWeights;

    setup(cx);
    let appearance = AppearancePreference {
        ui_font_weight: 600,
        code_font_weight: 350,
        ..AppearancePreference::default()
    };
    cx.update(|cx| {
        assert_eq!(FontWeights::get(cx).ui, gpui_kit::FontWeight(400.));
        appearance.apply(WindowAppearance::Light, cx);
        let weights = FontWeights::get(cx);
        assert_eq!(
            (weights.ui, weights.code),
            (gpui_kit::FontWeight(600.), gpui_kit::FontWeight(350.))
        );
    });
}
