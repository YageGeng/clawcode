//! "Open in": which external editors are installed, and what a saved choice resolves to.

use std::os::unix::fs::PermissionsExt as _;

use desktop::state::OpenIn;
use desktop::ui::EditorCatalog;

/// Writes an executable file named `name` in `dir`.
fn executable(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\n").expect("file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("permissions");
}

#[test]
fn detect_finds_editors_on_path_in_order() {
    let first = tempfile::tempdir().expect("dir");
    let second = tempfile::tempdir().expect("dir");
    executable(first.path(), "code");
    executable(second.path(), "zed");
    // A plain file is not a command.
    std::fs::write(second.path().join("cursor"), "").expect("file");
    let path =
        std::env::join_paths([first.path(), second.path()]).expect("PATH");

    let catalog = EditorCatalog::detect(Some(&path));
    assert_eq!(catalog.editors(), ["zed", "code"]);
    assert!(catalog.contains("code"));
    assert!(!catalog.contains("cursor"));
    assert!(EditorCatalog::detect(None).editors().is_empty());
}

#[test]
fn missing_saved_editor_falls_back_to_file_manager() {
    let dir = tempfile::tempdir().expect("dir");
    executable(dir.path(), "zed");
    let catalog = EditorCatalog::detect(Some(dir.path().as_os_str()));

    let zed = OpenIn::Editor("zed".to_string());
    assert_eq!(zed.resolve(&catalog), zed);
    assert_eq!(
        OpenIn::Editor("subl".to_string()).resolve(&catalog),
        OpenIn::FileManager
    );
    assert_eq!(OpenIn::FileManager.resolve(&catalog), OpenIn::FileManager);
}
