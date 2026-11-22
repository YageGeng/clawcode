//! The changed files of a patch as a folder tree, for the Changes tab's file
//! list: folders before files, each by name, and a folder that holds only one
//! folder shown together with it (`crates/acp`).

use std::collections::BTreeMap;

use typed_builder::TypedBuilder;

/// One row of the changed-files tree.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct ChangeNode {
    /// The file's or folder's name; merged folders join theirs with `/`.
    pub name: String,
    /// Path from the workspace root, as the patch names it.
    pub path: String,
    /// A folder's rows, in display order; empty for a file.
    #[builder(default)]
    pub children: Vec<ChangeNode>,
    /// A file's position in the patch; `None` for a folder.
    #[builder(default, setter(strip_option))]
    pub file: Option<usize>,
}

/// A folder while the tree is gathered: subfolders and files by name.
#[derive(Default)]
struct Folder {
    folders: BTreeMap<String, Folder>,
    files: BTreeMap<String, usize>,
}

impl Folder {
    /// Files `path` under this folder as the patch's file `index`.
    fn insert(&mut self, path: &str, index: usize) {
        match path.split_once('/') {
            Some((folder, rest)) => self
                .folders
                .entry(folder.to_owned())
                .or_default()
                .insert(rest, index),
            None => {
                self.files.insert(path.to_owned(), index);
            }
        }
    }

    /// The rows of this folder, whose path is `prefix` (empty at the root).
    fn into_nodes(self, prefix: &str) -> Vec<ChangeNode> {
        let join = |name: &str| {
            if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            }
        };
        let mut nodes = Vec::new();
        for (mut name, mut folder) in self.folders {
            // A folder holding just one folder reads as one row.
            while folder.files.is_empty() && folder.folders.len() == 1 {
                let Some((child, inner)) = folder.folders.pop_first() else {
                    break;
                };
                name = format!("{name}/{child}");
                folder = inner;
            }
            let path = join(&name);
            nodes.push(
                ChangeNode::builder()
                    .children(folder.into_nodes(&path))
                    .name(name)
                    .path(path)
                    .build(),
            );
        }
        nodes.extend(self.files.into_iter().map(|(name, index)| {
            ChangeNode::builder()
                .path(join(&name))
                .name(name)
                .file(index)
                .build()
        }));
        nodes
    }
}

impl ChangeNode {
    /// The tree of the patch's file `paths`, in patch order.
    pub fn tree<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<Self> {
        let mut root = Folder::default();
        for (index, path) in paths.into_iter().enumerate() {
            root.insert(path, index);
        }
        root.into_nodes("")
    }
}
