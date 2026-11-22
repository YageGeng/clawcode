//! `DesktopState` persistence: defaults, round trips, and damaged files.

use std::path::Path;

use desktop::state::{
    AppearancePreference, DesktopState, LanguagePreference, LayoutState,
    TabsState, ThemeModePreference,
};
use desktop::workspace::WorkspaceRegistry;

/// Writes raw text where the state file is expected.
fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write state file");
}

#[test]
fn missing_file_loads_defaults() {
    let dir = tempfile::tempdir().expect("temp dir");
    let state = DesktopState::load(&dir.path().join("desktop.json"));

    assert_eq!(state, DesktopState::default());
    assert_eq!(state.language, LanguagePreference::System);
    assert_eq!(state.appearance.light_theme, "Default Light");
    assert_eq!(state.appearance.dark_theme, "Default Dark");
    assert_eq!(state.version, DesktopState::VERSION);
}

#[test]
fn round_trip_preserves_every_field() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("desktop.json");
    let mut workspaces = WorkspaceRegistry::default();
    workspaces.add(Path::new("/tmp/w/a"));
    workspaces.remove(Path::new("/tmp/w/b"));
    let state = DesktopState::builder()
        .language(LanguagePreference::ZhCn)
        .appearance(
            AppearancePreference::builder()
                .mode(ThemeModePreference::Dark)
                .dark_theme("Catppuccin Mocha".to_string())
                .font_size(14.0)
                .build(),
        )
        .workspaces(workspaces)
        .layout(
            LayoutState::builder()
                .sidebar_open(false)
                .sidebar_width(300.0)
                .window_size(Some((1400.0, 900.0)))
                .build(),
        )
        .tabs(TabsState {
            open: vec!["s1".to_string(), "s2".to_string()],
            active: Some(1),
        })
        .build();

    state.save(&path).expect("save state");

    assert_eq!(DesktopState::load(&path), state);
}

#[test]
fn corrupt_file_is_backed_up_and_defaults_load() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("desktop.json");
    write(&path, "{not json");

    let state = DesktopState::load(&path);

    assert_eq!(state, DesktopState::default());
    let backup = std::fs::read_to_string(dir.path().join("desktop.json.bak"))
        .expect("backup kept");
    assert_eq!(backup, "{not json");
    assert!(!path.exists());
}

#[test]
fn partial_file_fills_missing_fields() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("desktop.json");
    write(&path, r#"{"version":1,"language":"zh-CN"}"#);

    let state = DesktopState::load(&path);

    assert_eq!(state.language, LanguagePreference::ZhCn);
    assert_eq!(state.layout, LayoutState::default());
    assert_eq!(state.appearance, AppearancePreference::default());
}

#[test]
fn unknown_enum_values_fall_back_to_system() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("desktop.json");
    write(&path, r#"{"language":"fr","appearance":{"mode":"sepia"}}"#);

    let state = DesktopState::load(&path);

    assert_eq!(state.language, LanguagePreference::System);
    assert_eq!(state.appearance.mode, ThemeModePreference::System);
    assert!(!dir.path().join("desktop.json.bak").exists());
}

#[test]
fn sidebar_width_is_clamped() {
    let width = |sidebar_width| {
        LayoutState::builder()
            .sidebar_width(sidebar_width)
            .build()
            .sidebar_width()
    };

    assert_eq!(width(1000.0), 400.0);
    assert_eq!(width(10.0), 200.0);
    assert_eq!(width(f32::NAN), 248.0);
    assert_eq!(width(-5.0), 200.0);
    assert_eq!(width(300.0), 300.0);
}

#[test]
fn save_creates_parent_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir
        .path()
        .join("nested")
        .join("clawcode")
        .join("desktop.json");
    let state = DesktopState::builder()
        .language(LanguagePreference::En)
        .build();

    state.save(&path).expect("save into a new directory");

    assert_eq!(DesktopState::load(&path).language, LanguagePreference::En);
}

#[test]
fn file_icon_themes_per_mode_default_read_back_and_tolerate_unknown_values() {
    use desktop::file::icons::Flavor;
    use desktop::state::{AppearancePreference, FileIconTheme};

    let defaults = AppearancePreference::default();
    assert_eq!(defaults.light_file_icons, FileIconTheme::Default);
    assert_eq!(defaults.dark_file_icons, FileIconTheme::Default);
    let read = |json: &str| -> AppearancePreference {
        serde_json::from_str(json).expect("appearance")
    };
    let chosen = read(
        r#"{"light_file_icons":"catppuccin_latte","dark_file_icons":"catppuccin_macchiato"}"#,
    );
    assert_eq!(chosen.light_file_icons, FileIconTheme::CatppuccinLatte);
    assert_eq!(chosen.dark_file_icons.flavor(), Some(Flavor::Macchiato));
    assert_eq!(FileIconTheme::Default.flavor(), None);
    // A value a newer build wrote falls back without losing the rest.
    let appearance = read(r#"{"dark_file_icons":"nerd","font_size":18.0}"#);
    assert_eq!(appearance.dark_file_icons, FileIconTheme::Default);
    assert_eq!(appearance.font_size, 18.0);
}

#[test]
fn old_state_file_with_sidebar_mode_still_loads() {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("desktop.json");
    write(
        &path,
        r#"{"version":1,"layout":{"sidebar_mode":"files"},"tabs":{"open":["s1"]}}"#,
    );
    let state = DesktopState::load(&path);
    assert_eq!(state.tabs.open, ["s1"]);
    assert!(!path.with_extension("json.bak").exists());
}

#[test]
fn font_settings_default_clamp_and_round_trip() {
    use desktop::state::AppearancePreference;

    let defaults = AppearancePreference::default();
    assert_eq!(
        (
            defaults.ui_font.clone(),
            defaults.ui_font_weight,
            defaults.code_font.clone(),
            defaults.code_font_size,
            defaults.code_font_weight,
        ),
        (None, 400, None, 14.0, 400)
    );
    let wild = AppearancePreference {
        ui_font_weight: 950,
        code_font_weight: 20,
        code_font_size: 8.0,
        font_size: f32::NAN,
        ..AppearancePreference::default()
    };
    let clamped = wild.clamped();
    assert_eq!(clamped.ui_font_weight, 900);
    assert_eq!(clamped.code_font_weight, 100);
    assert_eq!(clamped.code_font_size, 10.0);
    assert_eq!(clamped.font_size, 16.0);
    let nan = AppearancePreference {
        code_font_size: f32::NAN,
        ..AppearancePreference::default()
    };
    assert_eq!(nan.clamped().code_font_size, 14.0);

    let chosen = AppearancePreference {
        ui_font: Some("Inter".to_string()),
        code_font: Some("JetBrains Mono".to_string()),
        code_font_size: 18.0,
        ui_font_weight: 500,
        code_font_weight: 350,
        ..AppearancePreference::default()
    };
    let json = serde_json::to_string(&chosen).expect("json");
    let read: AppearancePreference =
        serde_json::from_str(&json).expect("appearance");
    assert_eq!(read, chosen);
}

#[test]
fn details_items_store_only_changes() {
    use desktop::state::DetailsItem;

    let mut state = DesktopState::default();
    assert!(state.details_item_visible(DetailsItem::Branch));
    assert!(!state.details_item_visible(DetailsItem::Context));
    state.set_details_item(DetailsItem::Branch, false);
    assert_eq!(state.details_items.len(), 1);
    assert!(!state.details_item_visible(DetailsItem::Branch));
    state.set_details_item(DetailsItem::Branch, true);
    assert!(state.details_items.is_empty());
    state.set_details_item(DetailsItem::Usage, true);
    assert_eq!(state.details_items.len(), 1);
    assert!(state.details_item_visible(DetailsItem::Usage));
}

#[test]
fn panel_state_drops_unknown_tabs_and_prunes_to_fifty() {
    use desktop::state::{PanelState, PanelTabKey};

    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("desktop.json");
    write(
        &path,
        r#"{"panels":{"session:a":{"open":true,"tabs":[{"kind":"files"},{"kind":"terminal"}],"active":3}}}"#,
    );
    let state = DesktopState::load(&path);
    let panel = state.panels.get("session:a").expect("panel");
    assert_eq!(panel.tabs, [PanelTabKey::Files]);
    assert_eq!(panel.active, Some(0));

    let mut state = DesktopState::default();
    for n in 0..60u64 {
        state.panels.insert(
            format!("session:{n}"),
            PanelState::builder().last_used_ms(n).build(),
        );
    }
    state.prune_panels(|_| true);
    assert_eq!(state.panels.len(), 50);
    assert!(!state.panels.contains_key("session:9"));
    assert!(state.panels.contains_key("session:10"));
    state.prune_panels(|key| key != "session:59");
    assert!(!state.panels.contains_key("session:59"));
}

#[test]
fn open_in_and_subagent_target_fall_back() {
    use desktop::state::{OpenIn, SubagentOpenTarget};

    let read = |json: &str| -> DesktopState {
        serde_json::from_str(json).expect("state")
    };
    assert_eq!(
        read(r#"{"open_in":"vim-ide"}"#).open_in,
        OpenIn::FileManager
    );
    assert_eq!(
        read(r#"{"open_in":{"editor":"zed"}}"#).open_in,
        OpenIn::Editor("zed".to_string())
    );
    assert_eq!(
        read(r#"{"subagent_open_target":"split"}"#).subagent_open_target,
        SubagentOpenTarget::Panel
    );
    assert_eq!(
        read(r#"{"subagent_open_target":"main"}"#).subagent_open_target,
        SubagentOpenTarget::Main
    );
}

#[test]
fn typed_font_weights_round_and_clamp() {
    use desktop::state::AppearancePreference;

    assert_eq!(AppearancePreference::font_weight_from(449.6), 450);
    assert_eq!(AppearancePreference::font_weight_from(975.0), 900);
    assert_eq!(AppearancePreference::font_weight_from(-3.0), 100);
}

#[test]
fn values_from_a_newer_build_keep_the_state_file() {
    use desktop::state::{DetailsItem, OpenIn};

    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("desktop.json");
    write(
        &path,
        r#"{"tabs":{"open":["s1"]},
            "details_items":{"cost":true,"branch":false},
            "open_in":{"remote":"x"}}"#,
    );
    let state = DesktopState::load(&path);
    assert!(
        !path.with_extension("json.bak").exists(),
        "the file was kept"
    );
    assert_eq!(state.tabs.open, ["s1"]);
    assert_eq!(state.details_items.len(), 1);
    assert_eq!(state.details_items.get(&DetailsItem::Branch), Some(&false));
    assert_eq!(state.open_in, OpenIn::FileManager);
}

#[test]
fn timeline_visibility_is_saved() {
    assert!(LayoutState::default().timeline_open);
    let layout = LayoutState::builder().timeline_open(false).build();
    let json = serde_json::to_string(&layout).expect("json");
    let back: LayoutState = serde_json::from_str(&json).expect("layout");
    assert!(!back.timeline_open);
    let older: LayoutState =
        serde_json::from_str(r#"{"sidebar_open":true}"#).expect("layout");
    assert!(older.timeline_open, "a missing value shows the overview");
}

#[test]
fn record_panel_tab_round_trips() {
    let json = serde_json::to_value(desktop::state::PanelTabKey::Record)
        .expect("json");
    assert_eq!(json, serde_json::json!({"kind": "record"}));
    let back: desktop::state::PanelTabKey =
        serde_json::from_value(json).expect("tab");
    assert_eq!(back, desktop::state::PanelTabKey::Record);
}
