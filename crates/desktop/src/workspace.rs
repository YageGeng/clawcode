//! Registered workspaces: project directories the sidebar groups sessions by.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Display name of a workspace directory: its folder name, or the whole path for a root.
pub fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// One registered workspace directory and whether its session list is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    /// Lexically normalized directory path.
    pub path: PathBuf,
    /// Whether the sidebar shows this workspace's sessions.
    pub expanded: bool,
}

/// Ordered workspace list plus the paths the user removed from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceRegistry {
    entries: Vec<WorkspaceEntry>,
    /// Removed paths stay here so session discovery does not re-add them.
    hidden: BTreeSet<PathBuf>,
}

impl WorkspaceRegistry {
    /// Returns the workspaces in sidebar order.
    pub fn entries(&self) -> &[WorkspaceEntry] {
        &self.entries
    }

    /// Registers a workspace chosen by the user; returns `false` when it was already listed.
    pub fn add(&mut self, path: &Path) -> bool {
        let path = normalize(path);
        self.hidden.remove(&path);
        if self.contains(&path) {
            return false;
        }
        self.entries.push(WorkspaceEntry {
            path,
            expanded: true,
        });
        true
    }

    /// Drops a workspace from the list and keeps discovery from re-adding it.
    pub fn remove(&mut self, path: &Path) {
        let path = normalize(path);
        self.entries.retain(|entry| entry.path != path);
        self.hidden.insert(path);
    }

    /// Appends session directories that are neither listed nor hidden, collapsed, in first-seen order.
    pub fn merge_discovered<'a>(
        &mut self,
        cwds: impl IntoIterator<Item = &'a Path>,
    ) {
        for cwd in cwds {
            let path = normalize(cwd);
            if self.hidden.contains(&path) || self.contains(&path) {
                continue;
            }
            self.entries.push(WorkspaceEntry {
                path,
                expanded: false,
            });
        }
    }

    /// Opens or closes one workspace's session list.
    pub fn set_expanded(&mut self, path: &Path, expanded: bool) {
        let path = normalize(path);
        if let Some(entry) =
            self.entries.iter_mut().find(|entry| entry.path == path)
        {
            entry.expanded = expanded;
        }
    }

    /// Moves a workspace by `offset` places; returns `false` when it is not
    /// listed or the move would leave the list.
    pub fn move_entry(&mut self, path: &Path, offset: isize) -> bool {
        let path = normalize(path);
        let Some(from) =
            self.entries.iter().position(|entry| entry.path == path)
        else {
            return false;
        };
        let Some(to) = from
            .checked_add_signed(offset)
            .filter(|to| *to < self.entries.len())
        else {
            return false;
        };
        let entry = self.entries.remove(from);
        self.entries.insert(to, entry);
        true
    }

    /// Whether the path, after normalization, is a listed workspace.
    pub fn contains(&self, path: &Path) -> bool {
        let path = normalize(path);
        self.entries.iter().any(|entry| entry.path == path)
    }
}

/// Lexically normalizes a path so equivalent spellings compare equal.
///
/// Drops `.` components and trailing separators and pops one segment for each
/// `..`; it never touches the filesystem, so removed directories still match.
pub fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Keep leading `..` on relative paths, which have nothing to pop.
                if !normalized.pop() {
                    normalized.push(component);
                }
            }
            other => normalized.push(other),
        }
    }
    normalized
}
