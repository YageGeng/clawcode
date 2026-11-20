//! Locale resolution, locale file completeness, and runtime switching.

use desktop::i18n::LocaleGeneration;
use desktop::state::LanguagePreference;
use gpui_kit::TestAppContext;

// `t!` resolves against the calling crate, so the test crate loads the same file.
rust_i18n::i18n!("locales", fallback = "en");

#[test]
fn resolve_maps_system_locales() {
    use LanguagePreference::*;
    for (preference, system, expected) in [
        (System, Some("zh-CN"), "zh-CN"),
        (System, Some("zh_TW.UTF-8"), "zh-CN"),
        (System, Some("zh-Hans"), "zh-CN"),
        (System, Some("en-US"), "en"),
        (System, Some("fr-FR"), "en"),
        (System, None, "en"),
        (En, Some("zh-CN"), "en"),
        (ZhCn, None, "zh-CN"),
    ] {
        assert_eq!(
            preference.resolve(system),
            expected,
            "{preference:?} with {system:?}"
        );
    }
}

#[test]
fn every_key_has_both_languages() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/locales/app.yml"
    ))
    .expect("locale file");
    let root: serde_yaml_ng::Mapping =
        serde_yaml_ng::from_str(&text).expect("valid yaml");
    let mut keys = 0;
    for (area, entries) in root
        .iter()
        .filter(|(key, _)| key.as_str() != Some("_version"))
    {
        let entries = entries.as_mapping().expect("area is a mapping");
        for (key, languages) in entries {
            keys += 1;
            for tag in ["en", "zh-CN"] {
                let value = languages
                    .get(tag)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                assert!(
                    !value.trim().is_empty(),
                    "{area:?}.{key:?} has no {tag} text"
                );
            }
        }
    }
    assert!(keys > 40, "only {keys} keys found");
}

#[gpui_kit::test]
fn apply_switches_component_and_app_strings(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let before = cx
            .try_global::<LocaleGeneration>()
            .map_or(0, |generation| generation.0);

        LanguagePreference::ZhCn.apply(cx);
        assert_eq!(rust_i18n::t!("Sidebar.new_session"), "新会话");
        assert_eq!(&*gpui_kit::component::locale(), "zh-CN");
        assert_eq!(cx.global::<LocaleGeneration>().0, before + 1);

        LanguagePreference::En.apply(cx);
        assert_eq!(rust_i18n::t!("Sidebar.new_session"), "New session");
    });
}
