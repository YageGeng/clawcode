//! The changed files of a patch as a folder tree, for the Changes tab's file
//! list: folders before files, each by name, and a folder that holds only one
//! folder shown together with it (`crates/acp`).

use std::collections::{BTreeMap, HashMap, HashSet};

use protocol::{WorkspaceChange, WorkspaceChangeKind, WorkspaceChanges};

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

/// Which files of a change list have their patch, and which are on the way.
///
/// The Changes tab loads patches file by file as their headers come into
/// view (M4b spec §4.3): a batch of at most [`PATCH_BATCH`] files at a time,
/// and a file the user picks at once.
#[derive(Debug, Default)]
pub struct PatchLoader {
    /// Every changed file in list order.
    changes: Vec<WorkspaceChange>,
    /// Each file's place in `changes`, by path.
    positions: HashMap<String, usize>,
    /// Files whose patch arrived.
    loaded: HashSet<String>,
    /// Files whose patch was requested and has not arrived.
    loading: HashSet<String>,
}

/// Most files one scrolling batch requests.
pub const PATCH_BATCH: usize = 4;

impl PatchLoader {
    /// A loader for `changes`, in their order, with nothing loaded.
    pub fn new(changes: &WorkspaceChanges) -> Self {
        Self {
            positions: changes
                .changes
                .iter()
                .enumerate()
                .map(|(index, change)| (change.path.clone(), index))
                .collect(),
            changes: changes.changes.clone(),
            ..Self::default()
        }
    }

    /// The changed files in list order.
    pub fn changes(&self) -> &[WorkspaceChange] {
        &self.changes
    }

    /// One changed file by path.
    pub fn change(&self, path: &str) -> Option<&WorkspaceChange> {
        self.changes.get(*self.positions.get(path)?)
    }

    /// One header-only patch per file, in list order, each parsed into the
    /// placeholder the diff shows until that file's patch arrives. Apart, a
    /// name git cannot write into a header spoils only its own.
    pub fn placeholder_patches(&self) -> Vec<String> {
        self.changes
            .iter()
            .map(|change| {
                let path = &change.path;
                let before = change.original_path.as_deref().unwrap_or(path);
                let mut patch = format!("diff --git a/{before} b/{path}\n");
                match change.kind {
                    WorkspaceChangeKind::Added
                    | WorkspaceChangeKind::Untracked => {
                        patch.push_str("new file mode 100644\n");
                    }
                    WorkspaceChangeKind::Deleted => {
                        patch.push_str("deleted file mode 100644\n");
                    }
                    WorkspaceChangeKind::Renamed => {
                        patch.push_str(&format!(
                            "rename from {before}\nrename to {path}\n"
                        ));
                    }
                    WorkspaceChangeKind::Modified
                    | WorkspaceChangeKind::Conflicted => {}
                }
                patch
            })
            .collect()
    }

    /// Files to request after the `seen` headers showed: within the window
    /// from the first seen file to [`PATCH_BATCH`] files past the last one,
    /// those neither loaded nor loading, at most [`PATCH_BATCH`]; nothing
    /// while a batch is loading. Once the window is loaded, loading stops
    /// until other files come into view.
    pub fn next_batch(&mut self, seen: &[String]) -> Vec<String> {
        if !self.loading.is_empty() {
            return Vec::new();
        }
        let positions =
            || seen.iter().filter_map(|path| self.positions.get(path));
        let (Some(&first), Some(&last)) =
            (positions().min(), positions().max())
        else {
            return Vec::new();
        };
        let batch: Vec<String> = self
            .changes
            .iter()
            .take(last + PATCH_BATCH + 1)
            .skip(first)
            .map(|change| &change.path)
            .filter(|path| !self.loaded.contains(*path))
            .take(PATCH_BATCH)
            .cloned()
            .collect();
        self.loading.extend(batch.iter().cloned());
        batch
    }

    /// Marks a file the user asked for as loading, whatever else is; `false`
    /// when it is loaded or loading already.
    pub fn request_now(&mut self, path: &str) -> bool {
        if self.loaded.contains(path) || self.loading.contains(path) {
            return false;
        }
        self.loading.insert(path.to_owned())
    }

    /// The path a renamed file had before, which its diff needs.
    pub fn original(&self, path: &str) -> Option<&str> {
        self.change(path)?.original_path.as_deref()
    }

    /// Records that a file's patch arrived, or failed for good.
    pub fn finished(&mut self, path: &str) {
        self.loading.remove(path);
        self.loaded.insert(path.to_owned());
    }

    /// Forgets every patch, arrived or on the way, so each loads again.
    pub fn forget_all(&mut self) {
        self.loaded.clear();
        self.loading.clear();
    }

    /// The earliest of `seen` in list order, to keep in view after the
    /// files are replaced.
    pub fn anchor(&self, seen: &[String]) -> Option<String> {
        seen.iter()
            .filter_map(|path| Some((self.positions.get(path)?, path)))
            .min()
            .map(|(_, path)| path.clone())
    }
}
