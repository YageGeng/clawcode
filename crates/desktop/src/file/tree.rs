//! The Files sidebar's model: one workspace's directories, listed lazily as
//! they are expanded, turned into nodes the tree renders.
//!
//! It knows nothing about gpui: the UI converts [`FileNode`]s into gpui-kit
//! tree items, which treat an item as a folder only when it has children, so
//! every directory carries a placeholder child until its listing is known.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use protocol::{WorkspaceDirListing, WorkspaceEntryKind};
use typed_builder::TypedBuilder;

/// Why a placeholder row stands where entries would be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderKind {
    /// The directory has not been listed yet.
    Loading,
    /// The directory has no entries to show.
    Empty,
    /// The listing stopped at the backend's entry limit.
    Truncated,
}

/// What one tree row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileNodeKind {
    /// A directory; its children follow.
    Directory,
    /// A file or a symbolic link, opened as a preview.
    File,
    /// A disabled note row.
    Placeholder(PlaceholderKind),
}

/// One row of the tree with its subtree.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub struct FileNode {
    /// Root-relative path; placeholders use `{dir}/\0` so they never collide with a file.
    pub path: String,
    /// Shown name.
    pub name: String,
    /// What the row is.
    pub kind: FileNodeKind,
    /// Whether a directory's children are shown.
    #[builder(default)]
    pub expanded: bool,
    /// Rows under a directory.
    #[builder(default)]
    pub children: Vec<FileNode>,
}

/// The listed directories of one workspace and which of them are expanded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTree {
    root: PathBuf,
    listings: HashMap<String, WorkspaceDirListing>,
    expanded: HashSet<String>,
}

impl FileTree {
    /// An empty tree of the workspace at `root`; nothing is listed yet.
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }

    /// The workspace directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stores one directory's listing; `dir` is root-relative, empty for the root.
    pub fn set_listing(&mut self, dir: &str, listing: WorkspaceDirListing) {
        self.listings.insert(dir.to_owned(), listing);
    }

    /// Whether `dir` still needs to be listed.
    pub fn needs_listing(&self, dir: &str) -> bool {
        !self.listings.contains_key(dir)
    }

    /// Marks a directory expanded or collapsed.
    pub fn set_expanded(&mut self, dir: &str, expanded: bool) {
        if expanded {
            self.expanded.insert(dir.to_owned());
        } else {
            self.expanded.remove(dir);
        }
    }

    /// The root and every expanded directory, parents before their children,
    /// in the order a refresh lists them.
    pub fn expanded_dirs(&self) -> Vec<String> {
        let mut dirs: Vec<String> = self.expanded.iter().cloned().collect();
        dirs.sort_by_key(|dir| (dir.matches('/').count(), dir.clone()));
        std::iter::once(String::new()).chain(dirs).collect()
    }

    /// Forgets every listing but keeps what is expanded, so a refresh lists it again.
    pub fn clear(&mut self) {
        self.listings.clear();
    }

    /// The rows of the root directory with their subtrees.
    pub fn nodes(&self) -> Vec<FileNode> {
        self.children_of("")
    }

    /// The rows under one directory: its entries, or a placeholder in their place.
    fn children_of(&self, dir: &str) -> Vec<FileNode> {
        let Some(listing) = self.listings.get(dir) else {
            return vec![Self::placeholder(dir, PlaceholderKind::Loading)];
        };
        if listing.entries.is_empty() {
            return vec![Self::placeholder(dir, PlaceholderKind::Empty)];
        }
        let mut nodes: Vec<FileNode> = listing
            .entries
            .iter()
            .map(|entry| match entry.kind {
                WorkspaceEntryKind::Directory => FileNode::builder()
                    .path(entry.path.clone())
                    .name(entry.name.clone())
                    .kind(FileNodeKind::Directory)
                    .expanded(self.expanded.contains(&entry.path))
                    .children(self.children_of(&entry.path))
                    .build(),
                WorkspaceEntryKind::File | WorkspaceEntryKind::Symlink => {
                    FileNode::builder()
                        .path(entry.path.clone())
                        .name(entry.name.clone())
                        .kind(FileNodeKind::File)
                        .build()
                }
            })
            .collect();
        if listing.truncated {
            nodes.push(Self::placeholder(dir, PlaceholderKind::Truncated));
        }
        nodes
    }

    /// A disabled row under `dir`.
    fn placeholder(dir: &str, kind: PlaceholderKind) -> FileNode {
        FileNode::builder()
            .path(format!("{dir}/\0"))
            .name(String::new())
            .kind(FileNodeKind::Placeholder(kind))
            .build()
    }
}
