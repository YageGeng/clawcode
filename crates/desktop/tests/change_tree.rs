//! The changed files as a folder tree: folders before files, each by name,
//! and a folder holding only one folder shown as one row.

use desktop::file::ChangeNode;

/// Each node as `name` or `name[children]`, files carrying their index.
fn outline(nodes: &[ChangeNode]) -> String {
    nodes
        .iter()
        .map(|node| match node.file {
            Some(index) => format!("{}#{index}", node.name),
            None => format!("{}[{}]", node.name, outline(&node.children)),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Folders lead, chains of lone folders merge, and files keep their place
/// in the patch.
#[test]
fn files_group_into_folders() {
    let tree = ChangeNode::tree([
        "README.md",
        "crates/acp/src/b.rs",
        "crates/acp/src/a.rs",
        "crates/acp/tests/x.rs",
        "Cargo.toml",
    ]);
    assert_eq!(
        outline(&tree),
        "crates/acp[src[a.rs#2 b.rs#1] tests[x.rs#3]] Cargo.toml#4 README.md#0"
    );
    let folder = &tree[0];
    assert_eq!(folder.path, "crates/acp");
    assert_eq!(folder.children[0].children[0].path, "crates/acp/src/a.rs");
}

/// A lone folder with a file beside its folder is not merged.
#[test]
fn a_folder_with_files_stays_its_own_row() {
    let tree = ChangeNode::tree(["a/x.rs", "a/b/y.rs"]);
    assert_eq!(outline(&tree), "a[b[y.rs#1] x.rs#0]");
}

/// A change list of these `(path, kind)` files.
fn changes(
    files: &[(&str, protocol::WorkspaceChangeKind)],
) -> protocol::WorkspaceChanges {
    protocol::WorkspaceChanges::builder()
        .changes(
            files
                .iter()
                .map(|(path, kind)| {
                    protocol::WorkspaceChange::builder()
                        .path((*path).to_owned())
                        .original_path(
                            (*kind == protocol::WorkspaceChangeKind::Renamed)
                                .then(|| format!("old-{path}")),
                        )
                        .kind(*kind)
                        .build()
                })
                .collect(),
        )
        .build()
}

/// A loader over files `a` to `f`, all modified.
fn loader() -> desktop::file::PatchLoader {
    use protocol::WorkspaceChangeKind::Modified;
    desktop::file::PatchLoader::new(&changes(&[
        ("a", Modified),
        ("b", Modified),
        ("c", Modified),
        ("d", Modified),
        ("e", Modified),
        ("f", Modified),
    ]))
}

/// The paths as owned strings.
fn paths(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

/// The placeholder patch parses into one empty file per change, in order,
/// with each change's status.
#[test]
fn placeholder_patch_parses_into_one_file_each() {
    use gpui_kit::component::diff::{DiffFile, DiffFileStatus};
    use protocol::WorkspaceChangeKind::{Added, Deleted, Modified, Renamed};
    let loader = desktop::file::PatchLoader::new(&changes(&[
        ("m.rs", Modified),
        ("n.rs", Added),
        ("d.rs", Deleted),
        ("r.rs", Renamed),
    ]));

    let files: Vec<DiffFile> = loader
        .placeholder_patches()
        .iter()
        .map(|patch| {
            DiffFile::parse(patch)
                .expect("parse")
                .into_iter()
                .next()
                .expect("one file")
        })
        .collect();
    let parsed: Vec<_> = files
        .iter()
        .map(|file| {
            (
                file.path().to_string(),
                file.status(),
                file.additions() + file.deletions(),
            )
        })
        .collect();
    assert_eq!(
        parsed,
        vec![
            ("m.rs".to_owned(), DiffFileStatus::Modified, 0),
            ("n.rs".to_owned(), DiffFileStatus::Added, 0),
            ("d.rs".to_owned(), DiffFileStatus::Deleted, 0),
            ("r.rs".to_owned(), DiffFileStatus::Renamed, 0),
        ]
    );
}

/// A seen file and those after it load, four at a time.
#[test]
fn patch_loader_batches_four_from_the_seen_file() {
    let mut loader = loader();
    assert_eq!(
        loader.next_batch(&paths(&["b"])),
        paths(&["b", "c", "d", "e"])
    );
}

/// No new batch starts while one is loading.
#[test]
fn patch_loader_waits_for_the_batch_in_flight() {
    let mut loader = loader();
    let first = loader.next_batch(&paths(&["b"]));
    loader.finished("b");
    assert!(loader.next_batch(&paths(&["f"])).is_empty());
    for path in &first[1..] {
        loader.finished(path);
    }
    assert_eq!(loader.next_batch(&paths(&["f"])), paths(&["f"]));
}

/// Files already loaded are skipped.
#[test]
fn patch_loader_skips_loaded_files() {
    let mut loader = loader();
    assert!(loader.request_now("c"));
    assert!(loader.request_now("d"));
    loader.finished("c");
    loader.finished("d");
    assert_eq!(loader.next_batch(&paths(&["b"])), paths(&["b", "e", "f"]));
}

/// A file asked for directly loads at once, but only once.
#[test]
fn patch_loader_request_now_ignores_the_batch() {
    let mut loader = loader();
    loader.next_batch(&paths(&["a"]));
    assert!(loader.request_now("f"));
    assert!(!loader.request_now("f"));
}

/// The earliest seen file in list order keeps the view in place.
#[test]
fn patch_loader_anchor_is_the_earliest_seen() {
    assert_eq!(loader().anchor(&paths(&["d", "b"])), Some("b".to_owned()));
}

/// Loading stays within the files in view and the four after the last of
/// them; once those are loaded, nothing more loads until others come into view.
#[test]
fn patch_loader_stops_past_the_window() {
    use protocol::WorkspaceChangeKind::Modified;
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
    let files: Vec<(&str, protocol::WorkspaceChangeKind)> =
        names.iter().map(|name| (*name, Modified)).collect();
    let mut loader = desktop::file::PatchLoader::new(&changes(&files));
    for path in loader.next_batch(&paths(&["b", "c"])) {
        loader.finished(&path);
    }

    // b and c are in view: their window runs to g.
    assert_eq!(loader.next_batch(&paths(&["b", "c"])), paths(&["f", "g"]));
    loader.finished("f");
    loader.finished("g");
    assert!(loader.next_batch(&paths(&["b", "c"])).is_empty());
}

/// Each file has a placeholder of its own, so a name git cannot write into
/// a patch header spoils only that file's, not the others'.
#[test]
fn placeholder_patches_keep_an_odd_name_to_itself() {
    use gpui_kit::component::diff::DiffFile;
    use protocol::WorkspaceChangeKind::Modified;
    let loader = desktop::file::PatchLoader::new(&changes(&[
        ("a.rs", Modified),
        ("odd\nname.rs", Modified),
        ("b.rs", Modified),
    ]));

    let patches = loader.placeholder_patches();
    assert_eq!(patches.len(), 3);
    for (index, path) in [(0, "a.rs"), (2, "b.rs")] {
        let files = DiffFile::parse(&patches[index]).expect("parse");
        assert_eq!(files[0].path().as_ref(), path);
    }
}
