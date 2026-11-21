//! The file tree's data model: lazily listed directories and their placeholders.

use desktop::file::tree::{FileNodeKind, FileTree, PlaceholderKind};
use protocol::{WorkspaceDirEntry, WorkspaceDirListing, WorkspaceEntryKind};

use WorkspaceEntryKind::{Directory, File};

/// A listing of `dir` holding `names`, with root-relative paths.
fn listing(
    names: &[(&str, WorkspaceEntryKind)],
    dir: &str,
) -> WorkspaceDirListing {
    let entries = names
        .iter()
        .map(|(name, kind)| WorkspaceDirEntry {
            name: (*name).to_owned(),
            path: if dir.is_empty() {
                (*name).to_owned()
            } else {
                format!("{dir}/{name}")
            },
            kind: *kind,
        })
        .collect();
    WorkspaceDirListing {
        entries,
        truncated: false,
    }
}

#[test]
fn unloaded_directories_render_as_folders_with_a_placeholder() {
    let mut tree = FileTree::new("/w".into());
    tree.set_listing("", listing(&[("src", Directory), ("a.rs", File)], ""));

    let nodes = tree.nodes();
    assert_eq!(nodes[0].path, "src");
    assert_eq!(nodes[0].kind, FileNodeKind::Directory);
    assert!(!nodes[0].expanded);
    assert_eq!(
        nodes[0].children[0].kind,
        FileNodeKind::Placeholder(PlaceholderKind::Loading)
    );
    assert_eq!(nodes[1].kind, FileNodeKind::File);
    assert!(nodes[1].children.is_empty());
    assert!(tree.needs_listing("src"));
}

#[test]
fn expanding_a_loaded_directory_shows_its_children() {
    let mut tree = FileTree::new("/w".into());
    tree.set_listing("", listing(&[("src", Directory)], ""));
    tree.set_listing("src", listing(&[("lib.rs", File)], "src"));
    tree.set_expanded("src", true);

    let nodes = tree.nodes();
    assert!(nodes[0].expanded);
    assert_eq!(nodes[0].children[0].path, "src/lib.rs");
    assert_eq!(tree.expanded_dirs(), ["", "src"]);
    assert!(!tree.needs_listing("src"));
}

#[test]
fn an_empty_directory_is_still_a_folder() {
    let mut tree = FileTree::new("/w".into());
    tree.set_listing("", listing(&[("empty", Directory)], ""));
    tree.set_listing("empty", listing(&[], "empty"));

    // An empty loaded folder keeps one placeholder child so it stays a folder.
    assert_eq!(
        tree.nodes()[0].children[0].kind,
        FileNodeKind::Placeholder(PlaceholderKind::Empty)
    );
}

#[test]
fn a_truncated_listing_ends_with_a_note() {
    let mut tree = FileTree::new("/w".into());
    let mut root = listing(&[("a.rs", File)], "");
    root.truncated = true;
    tree.set_listing("", root);

    let nodes = tree.nodes();
    assert_eq!(
        nodes.last().map(|node| node.kind.clone()),
        Some(FileNodeKind::Placeholder(PlaceholderKind::Truncated))
    );
}

#[test]
fn expanded_directories_are_listed_parents_first() {
    let mut tree = FileTree::new("/w".into());
    tree.set_expanded("src/ui", true);
    tree.set_expanded("src", true);
    tree.set_expanded("docs", true);
    tree.set_expanded("docs", false);

    assert_eq!(tree.expanded_dirs(), ["", "src", "src/ui"]);
    tree.clear();
    assert!(tree.needs_listing(""));
}
