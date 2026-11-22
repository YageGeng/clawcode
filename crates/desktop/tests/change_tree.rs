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
