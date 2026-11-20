//! Interface language selection, shared with gpui-kit's own component strings.

use gpui_kit::{App, Global};

use crate::state::LanguagePreference;

/// Locale tags shipped in `locales/app.yml`.
const ENGLISH: &str = "en";
const SIMPLIFIED_CHINESE: &str = "zh-CN";

/// Counts locale switches so views can refresh strings they cached in entities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocaleGeneration(pub u64);

impl Global for LocaleGeneration {}

impl LanguagePreference {
    /// Returns the locale tag to use, consulting the system locale for `System`.
    ///
    /// Every Chinese system locale maps to the only Chinese translation shipped.
    pub fn resolve(self, system_locale: Option<&str>) -> &'static str {
        match self {
            LanguagePreference::En => ENGLISH,
            LanguagePreference::ZhCn => SIMPLIFIED_CHINESE,
            LanguagePreference::System => match system_locale {
                Some(locale)
                    if locale.to_ascii_lowercase().starts_with("zh") =>
                {
                    SIMPLIFIED_CHINESE
                }
                _ => ENGLISH,
            },
        }
    }

    /// Switches app and component strings to this preference and redraws every window.
    pub fn apply(self, cx: &mut App) {
        let tag = self.resolve(sys_locale::get_locale().as_deref());
        // rust-i18n keeps one process-wide locale, so this also switches gpui-kit's strings.
        gpui_kit::component::set_locale(tag);
        let next = cx
            .try_global::<LocaleGeneration>()
            .map_or(1, |generation| generation.0 + 1);
        cx.set_global(LocaleGeneration(next));
        cx.refresh_windows();
        tracing::info!("interface language set to {}", tag);
    }
}
