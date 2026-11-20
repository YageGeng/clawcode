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
        .layout(LayoutState {
            sidebar_open: false,
            sidebar_width: 300.0,
            window_size: Some((1400.0, 900.0)),
        })
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
        LayoutState {
            sidebar_width,
            ..LayoutState::default()
        }
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
