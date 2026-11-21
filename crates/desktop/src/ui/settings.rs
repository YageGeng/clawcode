//! The settings dialog (M3b spec §7): language, appearance, fonts, and where
//! subagents open. Every change applies at once and is saved.

use gpui_kit::component::ThemeMode;
use gpui_kit::component::ThemeRegistry;
use gpui_kit::component::label::Label;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings,
};
use gpui_kit::component::{IndexPath, Sizable as _, WindowExt as _};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, Subscription, WeakEntity,
    Window, div, px, rems,
};

use super::AppView;
use crate::actions::OpenSettings;
use crate::state::{
    AppearancePreference, FileIconTheme, LanguagePreference,
    SubagentOpenTarget, ThemeModePreference,
};

/// A searchable list of font families: "Default" first, then every installed family.
type FontSelect = Entity<SelectState<Vec<SharedString>>>;

/// The settings dialog's shell state.
#[derive(Default)]
pub struct SettingsHost {
    /// Whether the dialog is open.
    pub(in crate::ui) open: bool,
    /// The font family pickers, built when the dialog first opens.
    pub(in crate::ui) font_pickers: Option<FontPickers>,
}

/// The two font family pickers of the Fonts page, built when the dialog first opens.
pub struct FontPickers {
    /// Interface font family.
    ui: FontSelect,
    /// Code font family.
    code: FontSelect,
    /// Choices written back to the appearance.
    _subscriptions: [Subscription; 2],
}

impl AppView {
    /// `OpenSettings`: shows the settings dialog.
    pub(super) fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        self.open_settings(window, cx);
    }

    /// Opens the settings dialog; when it is already open, nothing changes.
    ///
    /// The dialog gives focus back to whatever had it when it closes.
    pub fn open_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) {
        if self.settings.open {
            return;
        }
        self.settings.open = true;
        if self.settings.font_pickers.is_none() {
            self.settings.font_pickers =
                Some(self.build_font_pickers(window, cx));
        }
        let view = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, window, cx| {
            let viewport = window.viewport_size();
            // About 880×640, shrinking with the window and leaving 40px around it.
            let width = px(880.).min(viewport.width - px(80.));
            let height = px(560.).min(viewport.height - px(160.));
            let content = view
                .update(cx, |this, cx| this.render_settings(cx))
                .unwrap_or_else(|_| div().into_any_element());
            let closed = view.clone();
            dialog
                .title(Label::new(rust_i18n::t!("Settings.title").into_owned()))
                .w(width)
                .child(div().h(height).child(content))
                .on_close(move |_, _, cx| {
                    let _ = closed.update(cx, |this, cx| {
                        this.settings.open = false;
                        cx.notify();
                    });
                })
        });
        cx.notify();
    }

    /// Whether the settings dialog is open.
    pub fn settings_open(&self) -> bool {
        self.settings.open
    }

    /// Chooses where a plain click on a subagent opens it, and saves it.
    pub fn set_subagent_open_target(
        &mut self,
        target: SubagentOpenTarget,
        cx: &mut Context<'_, Self>,
    ) {
        self.state.subagent_open_target = target;
        self.persist(cx);
        cx.notify();
    }

    /// The font family pickers, preselecting the saved families, writing choices back.
    fn build_font_pickers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> FontPickers {
        let default = SharedString::from(
            rust_i18n::t!("Settings.font_default").into_owned(),
        );
        let families: Vec<SharedString> = std::iter::once(default.clone())
            .chain(
                crate::theme::InstalledFonts::get(cx)
                    .iter()
                    .map(|family| SharedString::from(family.clone())),
            )
            .collect();
        let appearance = &self.state.appearance;
        let picker = |saved: &Option<String>,
                      window: &mut Window,
                      cx: &mut Context<'_, Self>| {
            let selected = saved
                .as_ref()
                .and_then(|saved| {
                    families.iter().position(|family| family == saved)
                })
                .unwrap_or(0);
            let families = families.clone();
            cx.new(|cx| {
                SelectState::new(
                    families,
                    Some(IndexPath::new(selected)),
                    window,
                    cx,
                )
                .searchable(true)
            })
        };
        let ui = picker(&appearance.ui_font, window, cx);
        let code = picker(&appearance.code_font, window, cx);
        let choose = |field: fn(&mut AppearancePreference, Option<String>)| {
            let default = default.clone();
            move |this: &mut Self,
                  _: &FontSelect,
                  event: &SelectEvent<Vec<SharedString>>,
                  _: &mut Window,
                  cx: &mut Context<'_, Self>| {
                let SelectEvent::Confirm(Some(family)) = event else {
                    return;
                };
                let family = (*family != default).then(|| family.to_string());
                let mut appearance = this.state.appearance.clone();
                field(&mut appearance, family);
                this.set_appearance(appearance, cx);
            }
        };
        let subscriptions = [
            cx.subscribe_in(
                &ui,
                window,
                choose(|appearance, family| appearance.ui_font = family),
            ),
            cx.subscribe_in(
                &code,
                window,
                choose(|appearance, family| appearance.code_font = family),
            ),
        ];
        FontPickers {
            ui,
            code,
            _subscriptions: subscriptions,
        }
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
        // Out-of-range numbers are kept at their bounds (M3b spec §7.2).
        let appearance = appearance.clamped();
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

    /// Renders the settings pages whose fields read and write this view.
    fn render_settings(&mut self, cx: &mut Context<'_, Self>) -> AnyElement {
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
        let light_icons = Self::file_icon_field(&this, ThemeMode::Light);
        let dark_icons = Self::file_icon_field(&this, ThemeMode::Dark);
        let (ui_size_low, ui_size_high) =
            AppearancePreference::UI_FONT_SIZE_RANGE;
        let (code_size_low, code_size_high) =
            AppearancePreference::CODE_FONT_SIZE_RANGE;
        let ui_size = Self::number_field(
            &this,
            (ui_size_low, ui_size_high, 1.0),
            |appearance| appearance.font_size,
            |appearance, value| appearance.font_size = value,
        );
        let code_size = Self::number_field(
            &this,
            (code_size_low, code_size_high, 1.0),
            |appearance| appearance.code_font_size,
            |appearance, value| appearance.code_font_size = value,
        );
        let (weight_low, weight_high) = AppearancePreference::FONT_WEIGHT_RANGE;
        let weight_range =
            (f32::from(weight_low), f32::from(weight_high), 50.0);
        let ui_weight = Self::number_field(
            &this,
            weight_range,
            |appearance| f32::from(appearance.ui_font_weight),
            |appearance, value| {
                appearance.ui_font_weight =
                    AppearancePreference::font_weight_from(value)
            },
        );
        let code_weight = Self::number_field(
            &this,
            weight_range,
            |appearance| f32::from(appearance.code_font_weight),
            |appearance, value| {
                appearance.code_font_weight =
                    AppearancePreference::font_weight_from(value)
            },
        );
        let pickers = self
            .settings
            .font_pickers
            .as_ref()
            .map(|pickers| (pickers.ui.clone(), pickers.code.clone()));
        let font_field = |id: &'static str, picker: Option<FontSelect>| {
            SettingField::render(move |_, _, _| match &picker {
                Some(picker) => Select::new(picker)
                    .id(id)
                    .small()
                    .w(rems(14.))
                    .into_any_element(),
                None => div().into_any_element(),
            })
        };
        let (ui_picker, code_picker) = pickers.unzip();
        let ui_font = font_field("settings-ui-font", ui_picker);
        let code_font = font_field("settings-code-font", code_picker);
        let subagents = SettingField::dropdown(
            vec![
                ("panel".into(), t("Settings.subagents_panel")),
                ("main".into(), t("Settings.subagents_main")),
            ],
            {
                let this = this.clone();
                move |cx| {
                    let target = this
                        .upgrade()
                        .map(|this| this.read(cx).state.subagent_open_target)
                        .unwrap_or_default();
                    match target {
                        SubagentOpenTarget::Panel => "panel",
                        SubagentOpenTarget::Main => "main",
                    }
                    .into()
                }
            },
            {
                let this = this.clone();
                move |value, cx| {
                    let target = match value.as_ref() {
                        "main" => SubagentOpenTarget::Main,
                        _ => SubagentOpenTarget::Panel,
                    };
                    Self::update_from(&this, cx, |view, cx| {
                        view.set_subagent_open_target(target, cx)
                    });
                }
            },
        );
        Settings::new("settings")
            .pages([
                SettingPage::new(t("Settings.general")).group(
                    SettingGroup::new()
                        .item(SettingItem::new(
                            t("Settings.language"),
                            language,
                        ))
                        .item(SettingItem::new(
                            t("Settings.subagents"),
                            subagents,
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
                            t("Settings.light_file_icons"),
                            light_icons,
                        ))
                        .item(SettingItem::new(
                            t("Settings.dark_file_icons"),
                            dark_icons,
                        )),
                ),
                SettingPage::new(t("Settings.fonts")).group(
                    SettingGroup::new()
                        .item(SettingItem::new(t("Settings.ui_font"), ui_font))
                        .item(SettingItem::new(
                            t("Settings.font_size"),
                            ui_size,
                        ))
                        .item(SettingItem::new(
                            t("Settings.ui_font_weight"),
                            ui_weight,
                        ))
                        .item(SettingItem::new(
                            t("Settings.code_font"),
                            code_font,
                        ))
                        .item(SettingItem::new(
                            t("Settings.code_font_size"),
                            code_size,
                        ))
                        .item(SettingItem::new(
                            t("Settings.code_font_weight"),
                            code_weight,
                        )),
                ),
            ])
            .into_any_element()
    }

    /// A number field over one appearance value; written values are clamped by `set_appearance`.
    fn number_field(
        this: &WeakEntity<Self>,
        (min, max, step): (f32, f32, f32),
        read: fn(&AppearancePreference) -> f32,
        write: fn(&mut AppearancePreference, f32),
    ) -> SettingField<f64> {
        let reader = this.clone();
        let writer = this.clone();
        SettingField::number_input(
            NumberFieldOptions {
                min: f64::from(min),
                max: f64::from(max),
                step: f64::from(step),
            },
            move |cx| {
                reader.upgrade().map_or(f64::from(min), |this| {
                    f64::from(read(&this.read(cx).state.appearance))
                })
            },
            move |value, cx| {
                Self::update_appearance(&writer, cx, |appearance| {
                    write(appearance, value as f32)
                });
            },
        )
    }

    /// A dropdown over the file icon themes, choosing the one used in `mode`.
    ///
    /// Like the interface themes, every choice is offered in both modes.
    fn file_icon_field(
        this: &WeakEntity<Self>,
        mode: ThemeMode,
    ) -> SettingField<SharedString> {
        let light = mode == ThemeMode::Light;
        let options = FileIconTheme::ALL
            .into_iter()
            .map(|theme| {
                let label: SharedString = match theme {
                    FileIconTheme::Default => {
                        rust_i18n::t!("Settings.file_icons_default")
                            .into_owned()
                            .into()
                    }
                    FileIconTheme::CatppuccinLatte => "Catppuccin Latte".into(),
                    FileIconTheme::CatppuccinFrappe => {
                        "Catppuccin Frappé".into()
                    }
                    FileIconTheme::CatppuccinMacchiato => {
                        "Catppuccin Macchiato".into()
                    }
                    FileIconTheme::CatppuccinMocha => "Catppuccin Mocha".into(),
                };
                (theme.key().into(), label)
            })
            .collect();
        SettingField::dropdown(
            options,
            {
                let this = this.clone();
                move |cx| {
                    let theme = this
                        .upgrade()
                        .map(|this| {
                            let appearance = &this.read(cx).state.appearance;
                            if light {
                                appearance.light_file_icons
                            } else {
                                appearance.dark_file_icons
                            }
                        })
                        .unwrap_or_default();
                    theme.key().into()
                }
            },
            {
                let this = this.clone();
                move |value, cx| {
                    let theme = FileIconTheme::from_key(value.as_ref())
                        .unwrap_or_default();
                    Self::update_appearance(&this, cx, |appearance| {
                        if light {
                            appearance.light_file_icons = theme;
                        } else {
                            appearance.dark_file_icons = theme;
                        }
                    });
                }
            },
        )
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
