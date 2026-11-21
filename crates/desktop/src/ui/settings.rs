//! The singleton settings tab: language and appearance preferences.

use gpui_kit::component::ThemeMode;
use gpui_kit::component::ThemeRegistry;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings,
};
use gpui_kit::{
    AnyElement, Context, IntoElement as _, SharedString, WeakEntity, Window,
};

use super::{AppView, TabEntry};
use crate::actions::OpenSettings;
use crate::state::{
    AppearancePreference, LanguagePreference, ThemeModePreference,
};

impl AppView {
    /// `OpenSettings`: shows the settings tab.
    pub(super) fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        _: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.open_settings(cx);
    }

    /// Activates the settings tab, appending it when it is not open yet.
    pub fn open_settings(&mut self, cx: &mut Context<'_, Self>) {
        let index = self.tabs.open(TabEntry::Settings);
        self.activate(index, cx);
    }

    /// Switches the interface language, saves it, and re-measures open transcripts.
    pub fn set_language(
        &mut self,
        language: LanguagePreference,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.language = language;
        language.apply(cx);
        self.persist(cx);
        self.remeasure_views(cx);
        cx.notify();
    }

    /// Applies new appearance preferences, saves them, and re-measures open transcripts.
    pub fn set_appearance(
        &mut self,
        appearance: AppearancePreference,
        cx: &mut Context<'_, Self>,
    ) {
        appearance.apply(cx.window_appearance(), cx);
        self.state.appearance = appearance;
        self.persist(cx);
        self.remeasure_views(cx);
        cx.notify();
    }

    /// Re-measures every open transcript; fonts and strings change row heights.
    fn remeasure_views(&mut self, cx: &mut Context<'_, Self>) {
        for view in self.views.values() {
            view.update(cx, |view, cx| view.remeasure(cx));
        }
    }

    /// Renders the settings page whose fields read and write this view.
    pub(super) fn render_settings(
        &self,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        let this = cx.entity().downgrade();
        let t = |key: &str| SharedString::from(rust_i18n::t!(key).into_owned());
        let follow = t("Settings.follow_system");
        let language = SettingField::dropdown(
            vec![
                ("system".into(), follow.clone()),
                ("en".into(), "English".into()),
                ("zh-CN".into(), "简体中文".into()),
            ],
            {
                let this = this.clone();
                move |cx| {
                    let language = this
                        .upgrade()
                        .map(|this| this.read(cx).state.language)
                        .unwrap_or_default();
                    match language {
                        LanguagePreference::System => "system",
                        LanguagePreference::En => "en",
                        LanguagePreference::ZhCn => "zh-CN",
                    }
                    .into()
                }
            },
            {
                let this = this.clone();
                move |value, cx| {
                    let language = match value.as_ref() {
                        "en" => LanguagePreference::En,
                        "zh-CN" => LanguagePreference::ZhCn,
                        _ => LanguagePreference::System,
                    };
                    Self::update_from(&this, cx, |view, cx| {
                        view.set_language(language, cx)
                    });
                }
            },
        );
        let mode = SettingField::dropdown(
            vec![
                ("system".into(), follow),
                ("light".into(), t("Settings.light")),
                ("dark".into(), t("Settings.dark")),
            ],
            {
                let this = this.clone();
                move |cx| {
                    let mode = this
                        .upgrade()
                        .map(|this| this.read(cx).state.appearance.mode)
                        .unwrap_or_default();
                    match mode {
                        ThemeModePreference::System => "system",
                        ThemeModePreference::Light => "light",
                        ThemeModePreference::Dark => "dark",
                    }
                    .into()
                }
            },
            {
                let this = this.clone();
                move |value, cx| {
                    let mode = match value.as_ref() {
                        "light" => ThemeModePreference::Light,
                        "dark" => ThemeModePreference::Dark,
                        _ => ThemeModePreference::System,
                    };
                    Self::update_appearance(&this, cx, |appearance| {
                        appearance.mode = mode
                    });
                }
            },
        );
        let light = Self::theme_field(&this, ThemeMode::Light, cx);
        let dark = Self::theme_field(&this, ThemeMode::Dark, cx);
        let font_size = SettingField::number_input(
            NumberFieldOptions {
                min: 12.0,
                max: 20.0,
                step: 1.0,
            },
            {
                let this = this.clone();
                move |cx| {
                    this.upgrade().map_or(16.0, |this| {
                        f64::from(this.read(cx).state.appearance.font_size)
                    })
                }
            },
            move |value, cx| {
                Self::update_appearance(&this, cx, |appearance| {
                    appearance.font_size = value as f32
                });
            },
        );
        Settings::new("settings")
            .pages([
                SettingPage::new(t("Settings.general")).group(
                    SettingGroup::new().item(SettingItem::new(
                        t("Settings.language"),
                        language,
                    )),
                ),
                SettingPage::new(t("Settings.appearance")).group(
                    SettingGroup::new()
                        .item(SettingItem::new(t("Settings.mode"), mode))
                        .item(SettingItem::new(
                            t("Settings.light_theme"),
                            light,
                        ))
                        .item(SettingItem::new(t("Settings.dark_theme"), dark))
                        .item(SettingItem::new(
                            t("Settings.font_size"),
                            font_size,
                        )),
                ),
            ])
            .into_any_element()
    }

    /// A scrollable dropdown over the registered themes of one mode.
    fn theme_field(
        this: &WeakEntity<Self>,
        mode: ThemeMode,
        cx: &gpui_kit::App,
    ) -> SettingField<SharedString> {
        let options = ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .filter(|theme| theme.mode == mode)
            .map(|theme| (theme.name.clone(), theme.name.clone()))
            .collect();
        let light = mode == ThemeMode::Light;
        let reader = this.clone();
        let writer = this.clone();
        SettingField::scrollable_dropdown(
            options,
            move |cx| {
                reader.upgrade().map_or_else(SharedString::default, |this| {
                    let appearance = &this.read(cx).state.appearance;
                    if light {
                        appearance.light_theme.clone().into()
                    } else {
                        appearance.dark_theme.clone().into()
                    }
                })
            },
            move |value, cx| {
                Self::update_appearance(&writer, cx, |appearance| {
                    if light {
                        appearance.light_theme = value.to_string();
                    } else {
                        appearance.dark_theme = value.to_string();
                    }
                });
            },
        )
    }

    /// Runs `f` on the shell from a settings field callback, if the shell is still alive.
    fn update_from(
        this: &WeakEntity<Self>,
        cx: &mut gpui_kit::App,
        f: impl FnOnce(&mut Self, &mut Context<'_, Self>),
    ) {
        let _ = this.update(cx, |view, cx| f(view, cx));
    }

    /// Edits a copy of the appearance preferences and applies it.
    fn update_appearance(
        this: &WeakEntity<Self>,
        cx: &mut gpui_kit::App,
        edit: impl FnOnce(&mut AppearancePreference),
    ) {
        Self::update_from(this, cx, |view, cx| {
            let mut appearance = view.state.appearance.clone();
            edit(&mut appearance);
            view.set_appearance(appearance, cx);
        });
    }
}
