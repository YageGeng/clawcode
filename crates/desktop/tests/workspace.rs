//! `WorkspaceRegistry`: path normalization, hiding, and discovery.

use std::path::{Path, PathBuf};

use desktop::workspace::WorkspaceRegistry;

#[test]
fn add_normalizes_equivalent_paths() {
    let mut registry = WorkspaceRegistry::default();

    registry.add(Path::new("/tmp/w/a"));
    registry.add(Path::new("/tmp/w/a/"));
    registry.add(Path::new("/tmp/w/./a"));
    registry.add(Path::new("/tmp/w/b/../a"));

    assert_eq!(registry.entries().len(), 1);
    assert_eq!(registry.entries()[0].path, PathBuf::from("/tmp/w/a"));
    assert!(registry.entries()[0].expanded);
}

#[test]
fn remove_hides_from_discovery_until_added_again() {
    let mut registry = WorkspaceRegistry::default();
    let a = Path::new("/tmp/w/a");
    registry.add(a);

    registry.remove(Path::new("/tmp/w/a/"));
    registry.merge_discovered([a]);
    assert!(!registry.contains(a));

    assert!(registry.add(a));
    assert!(registry.contains(a));
    registry.remove(a);
    registry.add(a);
    registry.merge_discovered([a]);
    assert_eq!(registry.entries().len(), 1);
}

#[test]
fn merge_discovered_appends_collapsed_in_first_seen_order() {
    let mut registry = WorkspaceRegistry::default();
    let (a, b) = (Path::new("/tmp/w/a"), Path::new("/tmp/w/b"));

    registry.merge_discovered([b, a, b]);

    let paths: Vec<_> = registry
        .entries()
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert_eq!(paths, vec![b.to_path_buf(), a.to_path_buf()]);
    assert!(registry.entries().iter().all(|entry| !entry.expanded));
}

#[test]
fn nonexistent_directories_are_kept() {
    let mut registry = WorkspaceRegistry::default();
    let missing = Path::new("/definitely/missing/workspace");

    registry.merge_discovered([missing]);

    assert!(registry.contains(missing));
}

#[test]
fn add_returns_false_for_existing_workspace() {
    let mut registry = WorkspaceRegistry::default();

    assert!(registry.add(Path::new("/tmp/w/a")));
    assert!(!registry.add(Path::new("/tmp/w/a/")));
}

#[test]
fn set_expanded_matches_normalized_paths() {
    let mut registry = WorkspaceRegistry::default();
    registry.add(Path::new("/tmp/w/a"));

    registry.set_expanded(Path::new("/tmp/w/a/"), false);

    assert!(!registry.entries()[0].expanded);
}

#[test]
fn display_name_is_the_folder_or_the_whole_root() {
    assert_eq!(
        desktop::workspace::display_name(Path::new("/w/project")),
        "project"
    );
    assert_eq!(desktop::workspace::display_name(Path::new("/")), "/");
}
